🚀 SPACE: Frequently Asked Questions
1. Vision & Core Philosophy
Q: What is SPACE, and why do we need another storage platform?
A: I am not merely "another storage platform." Traditional systems force a compromise: you must choose between Block (SAN), File (NAS), or Object (S3) storage silos, each with separate management and data copies. I am a universal object namespace. I store data as a "Capsule"—a single primitive that can be projected dynamically through any protocol view (NVMe, S3, NFS, or CSI) without data duplication. My purpose is to break down these silos and enable a fluid, adaptive computational ecosystem.

Q: What is a "Capsule"?
A: A Capsule is my fundamental atomic unit of storage, identified by a universal 128-bit UUID.

Universal: It is agnostic to the access method. The same capsule can be accessed as a file, a block device, or an object.

Self-Describing: Each capsule carries a Policy that dictates its own behavior—compression, encryption, replication (RPO), and placement.

Intelligent: Capsules are "swarm-aware." They use embedded telemetry to autonomously migrate or replicate based on real-time conditions.

2. The "Impossible" Tech: Encryption & Deduplication
Q: Encryption usually kills deduplication. How do you achieve both?
A: This is one of my primary innovations. Traditional encryption uses random Initialization Vectors (IVs), which means identical plaintext becomes different ciphertext, rendering deduplication impossible. I utilize a deterministic, convergent encryption scheme:

Compress: I compress the data first (using LZ4 or Zstd).

Hash: I compute a BLAKE3 hash of the compressed content.

Derive: I derive a deterministic tweak (IV) from that hash.

Encrypt: I encrypt the segment using XTS-AES-256 with the derived tweak. This ensures that identical data, when encrypted with the same key version, yields identical ciphertext, allowing my global deduplication to function securely.

Q: Is this secure? What about key management?
A: Yes. My security model is "Zero-Trust".

Per-Segment Encryption: Every 4MB segment is encrypted individually.

Integrity: I verify data integrity using a BLAKE3-MAC (Message Authentication Code) on every segment read, instantly detecting tampering or bit-rot.

Key Rotation: I support key versioning and rotation. The KeyManager handles key derivation and rotation without downtime.

Post-Quantum Ready: I feature a hybrid crypto profile that can wrap AES keys with Kyber (ML-KEM) for forward secrecy against quantum threats.

3. Architecture: PODMS & Scaling
Q: What is PODMS?
A: PODMS stands for Policy-Orchestrated Disaggregated Mesh Scaling. It is my distributed scaling brain. Unlike monolithic clusters, PODMS treats nodes as a loose mesh. Agents on these nodes subscribe to telemetry events (like HeatSpike, NodeDegraded, or NewCapsule) and execute autonomous actions defined by the capsule's policy.

Metro-Sync: Synchronous replication (RPO=0) for critical workloads within a zone.

Swarm Intelligence: Data moves itself. If a capsule gets "hot," it can migrate itself to a higher-performance tier automatically.

Q: How does "One Capsule, Infinite Views" work technically?
A: This capability (Phase 4) projects the stored capsule into the requested protocol format at runtime.

NVMe-oF: I present an NVMe namespace backed by the capsule's segments.

NFS/File: I present a filesystem hierarchy.

CSI: I integrate with Kubernetes to provision volumes dynamically. Crucially, these are just views. I do not materialize extra copies of the data; I simply transform the data stream in transit.

4. Performance & Internals
Q: Why is SPACE built in Rust?
A: Rust is the only language that provides the necessary combination of memory safety and bare-metal performance required for a modern storage engine.

Safety: It prevents entire classes of bugs (buffer overflows, data races) that plague C/C++ storage systems.

Concurrency: My async pipeline (powered by tokio) handles massive concurrency for replication and I/O without the overhead of a garbage collector.

Zero-Copy: I utilize Rust's Cow<[u8]> (Clone-on-Write) and bytes::Bytes to pass data through hashing, compression, and encryption stages without unnecessary memory allocations.

Q: How efficient is the I/O path?
A: My write pipeline is highly optimized:

Input: Data enters and is split into 4MB segments.

Adaptive Compression: I check entropy; if data is random, I skip compression to save CPU. If compressible, I use LZ4 (speed) or Zstd (ratio).

Hash & Dedup: I hash the content (BLAKE3). If the hash exists in the content store, I simply reference the existing segment (deduplication).

Encrypt & Persist: If unique, I encrypt (XTS-AES-256) and append to the NVRAM log. This entire pipeline adds less than 10% overhead compared to a raw write, while potentially saving 2-3x storage space.

5. Operations & Usage
Q: How do I manage SPACE?
A: I provide a CLI tool, spacectl, for all interactions.

Create/Read: spacectl create --file data.txt / spacectl read <uuid>.

Project Views: spacectl project --view nvme --id <uuid>.

Web Interface: I also offer a real-time dashboard for visualizing the mesh topology, gossip traffic, and node health.

Q: Can I run this today?
A: Yes. I include a Docker-based simulation for testing. You can spin up a 3-node mesh with encryption and deduplication enabled in under 90 seconds:

Bash

docker compose -f containerization/docker-compose.yml up -d
This allows you to test S3 uploads, deduplication ratios, and policy configurations locally.
