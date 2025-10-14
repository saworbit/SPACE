use common::*;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Write, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, RwLock};
use anyhow::Result;
use serde_json;

pub struct NvramLog {
    file: Arc<RwLock<File>>,
    segment_map: Arc<RwLock<HashMap<SegmentId, Segment>>>,
    next_offset: Arc<RwLock<u64>>,
    metadata_path: String,
}

impl NvramLog {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let metadata_path = format!("{}.segments", path_str);
        
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path_str)?;

        // Get file size for next_offset
        let file_len = file.metadata()?.len();
        
        // Load segment map if exists
        let segment_map = if Path::new(&metadata_path).exists() {
            let data = std::fs::read_to_string(&metadata_path)?;
            serde_json::from_str(&data)?
        } else {
            HashMap::new()
        };

        Ok(Self {
            file: Arc::new(RwLock::new(file)),
            segment_map: Arc::new(RwLock::new(segment_map)),
            next_offset: Arc::new(RwLock::new(file_len)),
            metadata_path,
        })
    }

    fn save_segment_map(&self) -> Result<()> {
        let map = self.segment_map.read().unwrap();
        let json = serde_json::to_string_pretty(&*map)?;
        std::fs::write(&self.metadata_path, json)?;
        Ok(())
    }

    pub fn append(&self, seg_id: SegmentId, data: &[u8]) -> Result<Segment> {
        let mut file = self.file.write().unwrap();
        let mut next_offset = self.next_offset.write().unwrap();
        
        let offset = *next_offset;
        
        // Write to end of file
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;
        file.sync_data()?;  // fsync for durability
        
        let segment = Segment {
            id: seg_id,
            offset,
            len: data.len() as u32,
        };
        
        *next_offset += data.len() as u64;
        
        // Update segment map
        self.segment_map.write().unwrap()
            .insert(seg_id, segment.clone());
        
        drop(file);
        drop(next_offset);
        self.save_segment_map()?;  // Persist segment map
        
        Ok(segment)
    }

    pub fn read(&self, seg_id: SegmentId) -> Result<Vec<u8>> {
        let segment = self.segment_map.read().unwrap()
            .get(&seg_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Segment not found"))?;
        
        let mut file = self.file.write().unwrap();
        file.seek(SeekFrom::Start(segment.offset))?;
        
        let mut buffer = vec![0u8; segment.len as usize];
        file.read_exact(&mut buffer)?;
        
        Ok(buffer)
    }
}