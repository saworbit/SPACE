//! Rebuilding `space.db` from the journal gives the index it replaces, over
//! random histories of S3 PUTs, overwrites, DELETEs, batch deletes, bucket
//! creates and deletes, and data log and journal compactions (#139). Every
//! object's capsule has BLAKE3 of the bytes put as its digest.
//!
//! The invariant is `rebuild(journal, data log) == index` (docs/capsule.md).

use bytes::Bytes;
use capsule_registry::modular_pipeline::StoreSecurity;
use capsule_registry::CapsuleRegistry;
use futures::stream;
use nvram_sim::NvramLog;
use proptest::prelude::*;
use protocol_s3::{ListQuery, S3Error, S3View};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;
use test_support::TempDir;

const BUCKETS: [&str; 2] = ["bkt-a", "bkt-b"];
const KEYS: usize = 5;

/// Bodies the workload puts. Equal bodies are dedup hits. The two longest
/// share their first 4 MiB segment and differ in their last.
fn bodies() -> &'static [Vec<u8>] {
    static BODIES: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    BODIES.get_or_init(|| {
        let segment: Vec<u8> = (0..4u32 << 20)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let long = |tail: &[u8]| [segment.as_slice(), tail].concat();
        vec![
            Vec::new(),
            b"x".to_vec(),
            b"hello, space".to_vec(),
            (0..5000u32).map(|i| (i % 251) as u8).collect(),
            long(b"first tail"),
            long(b"second, longer tail"),
        ]
    })
}

#[derive(Debug, Clone)]
enum Op {
    CreateBucket(usize),
    DeleteBucket(usize),
    Put {
        bucket: usize,
        key: usize,
        body: usize,
    },
    Delete {
        bucket: usize,
        key: usize,
    },
    DeleteMany {
        bucket: usize,
        keys: Vec<usize>,
    },
    CompactLog,
    CompactJournal,
}

fn op() -> impl Strategy<Value = Op> {
    let bucket = 0..BUCKETS.len();
    prop_oneof![
        1 => bucket.clone().prop_map(Op::CreateBucket),
        1 => bucket.clone().prop_map(Op::DeleteBucket),
        6 => (bucket.clone(), 0..KEYS, 0..bodies().len())
            .prop_map(|(bucket, key, body)| Op::Put { bucket, key, body }),
        2 => (bucket.clone(), 0..KEYS).prop_map(|(bucket, key)| Op::Delete { bucket, key }),
        1 => (bucket, prop::collection::vec(0..KEYS, 1..4))
            .prop_map(|(bucket, keys)| Op::DeleteMany { bucket, keys }),
        1 => Just(Op::CompactLog),
        1 => Just(Op::CompactJournal),
    ]
}

fn key(k: usize) -> String {
    format!("dir/k{k}")
}

struct Store {
    registry: CapsuleRegistry,
    log: NvramLog,
    view: S3View,
}

fn open(dir: &Path) -> Store {
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    let view = S3View::new(registry.clone(), log.clone(), &StoreSecurity::none()).unwrap();
    Store {
        registry,
        log,
        view,
    }
}

/// Every tree of the index, in a form that compares.
#[derive(Debug, PartialEq)]
struct Index {
    capsules: Vec<String>,
    refs: Vec<(u64, u32)>,
    content: Vec<(String, u64)>,
    namespaces: Vec<(String, u64)>,
    names: Vec<Vec<String>>,
    capsule_names: Vec<(String, Vec<(String, String)>)>,
}

fn index(registry: &CapsuleRegistry) -> Index {
    let mut capsules: Vec<String> = registry
        .capsules()
        .iter()
        .map(|c| format!("{c:?}"))
        .collect();
    capsules.sort();
    let mut refs: Vec<(u64, u32)> = registry
        .segment_refs()
        .unwrap()
        .into_iter()
        .map(|(segment, count)| (segment.0, count))
        .collect();
    refs.sort();
    let mut content: Vec<(String, u64)> = registry
        .content_entries()
        .into_iter()
        .map(|(hash, segment)| (hash.0, segment.0))
        .collect();
    content.sort();
    let mut capsule_names: Vec<(String, Vec<(String, String)>)> = registry
        .capsules()
        .iter()
        .map(|c| {
            let mut held = registry.names_of(c.id).unwrap();
            held.sort();
            (c.id.as_uuid().to_string(), held)
        })
        .collect();
    capsule_names.sort();
    Index {
        capsules,
        refs,
        content,
        namespaces: registry.list_namespaces("", None, usize::MAX).unwrap(),
        names: BUCKETS
            .iter()
            .map(|b| {
                registry
                    .list_names(&format!("s3:{b}"), "", None, usize::MAX)
                    .unwrap()
                    .into_iter()
                    .map(|entry| format!("{entry:?}"))
                    .collect()
            })
            .collect(),
        capsule_names,
    }
}

/// What the workload expects: the buckets, and each object's body.
#[derive(Default)]
struct Model {
    buckets: BTreeSet<usize>,
    objects: BTreeMap<(usize, usize), usize>,
}

impl Model {
    fn holds(&self, bucket: usize) -> bool {
        self.objects.keys().any(|(b, _)| *b == bucket)
    }
}

async fn apply(store: &Store, model: &mut Model, op: &Op) {
    let view = &store.view;
    match op {
        Op::CreateBucket(b) => {
            view.create_bucket(BUCKETS[*b]).unwrap();
            model.buckets.insert(*b);
        }
        Op::DeleteBucket(b) => match view.delete_bucket(BUCKETS[*b]).await {
            Ok(()) => {
                assert!(model.buckets.remove(b) && !model.holds(*b), "{op:?}");
            }
            Err(S3Error::BucketNotEmpty(_)) => assert!(model.holds(*b), "{op:?}"),
            Err(S3Error::NoSuchBucket(_)) => assert!(!model.buckets.contains(b), "{op:?}"),
            Err(err) => panic!("{op:?}: {err}"),
        },
        Op::Put {
            bucket,
            key: k,
            body,
        } => {
            let data = Bytes::from_static(bodies()[*body].as_slice());
            let put = view
                .put_object(
                    BUCKETS[*bucket],
                    &key(*k),
                    stream::once(async move { Ok::<_, std::io::Error>(data) }),
                )
                .await;
            match put {
                Ok(_) => {
                    assert!(model.buckets.contains(bucket), "{op:?}");
                    model.objects.insert((*bucket, *k), *body);
                }
                Err(S3Error::NoSuchBucket(_)) => {
                    assert!(!model.buckets.contains(bucket), "{op:?}")
                }
                Err(err) => panic!("{op:?}: {err}"),
            }
        }
        Op::Delete { bucket, key: k } => match view.delete_object(BUCKETS[*bucket], &key(*k)).await
        {
            Ok(()) => {
                assert!(model.buckets.contains(bucket), "{op:?}");
                model.objects.remove(&(*bucket, *k));
            }
            Err(S3Error::NoSuchBucket(_)) => assert!(!model.buckets.contains(bucket), "{op:?}"),
            Err(err) => panic!("{op:?}: {err}"),
        },
        Op::DeleteMany { bucket, keys } => {
            let names: Vec<String> = keys.iter().map(|k| key(*k)).collect();
            match view.delete_objects(BUCKETS[*bucket], &names).await {
                Ok(()) => {
                    assert!(model.buckets.contains(bucket), "{op:?}");
                    for k in keys {
                        model.objects.remove(&(*bucket, *k));
                    }
                }
                Err(S3Error::NoSuchBucket(_)) => {
                    assert!(!model.buckets.contains(bucket), "{op:?}")
                }
                Err(err) => panic!("{op:?}: {err}"),
            }
        }
        Op::CompactLog => {
            store.log.compact().unwrap();
        }
        Op::CompactJournal => {
            store.registry.compact_journal().unwrap();
        }
    }
}

/// The store holds what the model says, and each object's capsule digest
/// is BLAKE3 of its body.
fn check(store: &Store, model: &Model) {
    for (b, bucket) in BUCKETS.iter().enumerate() {
        assert_eq!(
            store.view.head_bucket(bucket).is_ok(),
            model.buckets.contains(&b),
            "{bucket}"
        );
        if !model.buckets.contains(&b) {
            continue;
        }
        let listed: Vec<String> = store
            .view
            .list_objects(bucket, &ListQuery::default())
            .unwrap()
            .objects
            .into_iter()
            .map(|o| o.key)
            .collect();
        let expected: Vec<String> = model
            .objects
            .keys()
            .filter(|(ob, _)| *ob == b)
            .map(|(_, k)| key(*k))
            .collect();
        assert_eq!(listed, expected, "{bucket}");
    }
    for ((b, k), body) in &model.objects {
        let id = store
            .view
            .head_object(BUCKETS[*b], &key(*k))
            .unwrap()
            .capsule_id();
        let capsule = store.registry.get_capsule(id).unwrap().unwrap();
        assert_eq!(
            capsule.digest,
            Some(*blake3::hash(&bodies()[*body]).as_bytes()),
            "{}/{}",
            BUCKETS[*b],
            key(*k)
        );
    }
}

fn run(ops: &[Op]) {
    let dir = TempDir::new("s3_rebuild_prop");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let store = open(&dir);
        let mut model = Model::default();
        apply(&store, &mut model, &Op::CreateBucket(0)).await;
        for op in ops {
            apply(&store, &mut model, op).await;
        }
        check(&store, &model);
        let before = index(&store.registry);
        drop(store);

        std::fs::remove_dir_all(dir.join("space.db")).unwrap();
        let store = open(&dir);
        assert_eq!(index(&store.registry), before, "the rebuilt index differs");
        check(&store, &model);
    });
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 24,
        ..ProptestConfig::default()
    })]

    #[test]
    fn a_rebuilt_index_is_the_index_it_replaces(ops in prop::collection::vec(op(), 0..32)) {
        run(&ops);
    }
}
