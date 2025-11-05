#![no_main]

use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use encryption::xts::{decrypt_segment, encrypt_segment};
use libfuzzer_sys::fuzz_target;

const MIN_PLAINTEXT: usize = 16;

fuzz_target!(|data: &[u8]| {
    if data.len() < MASTER_KEY_SIZE + 16 + MIN_PLAINTEXT {
        return;
    }

    let mut master_key = [0u8; MASTER_KEY_SIZE];
    master_key.copy_from_slice(&data[..MASTER_KEY_SIZE]);
    let mut manager = match KeyManager::new(master_key) {
        Ok(manager) => manager,
        Err(_) => return,
    };

    let key_pair = match manager.get_key(1) {
        Ok(pair) => pair.clone(),
        Err(_) => return,
    };

    let mut tweak = [0u8; 16];
    tweak.copy_from_slice(&data[MASTER_KEY_SIZE..MASTER_KEY_SIZE + 16]);

    let payload = &data[MASTER_KEY_SIZE + 16..];
    if payload.len() < MIN_PLAINTEXT {
        return;
    }

    if let Ok((ciphertext, metadata)) = encrypt_segment(payload, key_pair.clone(), 1, tweak) {
        if let Ok(plaintext) = decrypt_segment(&ciphertext, key_pair, &metadata) {
            assert_eq!(payload, plaintext.as_slice());
        }
    }
});
