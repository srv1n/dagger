// Utility functions for task-core

/// Compute a simple 16-bit hash for a string
pub fn const_hash16(s: &str) -> u16 {
    let bytes = s.as_bytes();
    let mut hash: u16 = 0;
    
    for &byte in bytes {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u16);
    }
    
    hash
}