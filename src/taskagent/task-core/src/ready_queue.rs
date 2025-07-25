use crossbeam::queue::SegQueue;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A lock-free ready queue for task management with capacity control
pub struct ReadyQueue<T> {
    queue: SegQueue<T>,
    capacity: usize,
    size: Arc<AtomicUsize>,
}

impl<T> ReadyQueue<T> {
    /// Creates a new ReadyQueue with the specified capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: SegQueue::new(),
            capacity,
            size: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Attempts to push an item onto the queue
    /// Returns false if the queue is at capacity (backpressure)
    pub fn push(&self, item: T) -> bool {
        // Check capacity before attempting to push
        let current_size = self.size.load(Ordering::Acquire);
        if current_size >= self.capacity {
            return false;
        }

        // Try to increment size atomically
        // Use compare_exchange to ensure we don't exceed capacity
        match self.size.compare_exchange(
            current_size,
            current_size + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // Successfully incremented, push the item
                self.queue.push(item);
                true
            }
            Err(_) => {
                // Another thread modified size, retry by checking capacity again
                let new_size = self.size.load(Ordering::Acquire);
                if new_size >= self.capacity {
                    false
                } else {
                    // Try once more with fetch_add
                    if self.size.fetch_add(1, Ordering::AcqRel) < self.capacity {
                        self.queue.push(item);
                        true
                    } else {
                        // We exceeded capacity, revert the increment
                        self.size.fetch_sub(1, Ordering::AcqRel);
                        false
                    }
                }
            }
        }
    }

    /// Attempts to pop an item from the queue
    pub fn pop(&self) -> Option<T> {
        match self.queue.pop() {
            Some(item) => {
                // Decrement size after successful pop
                self.size.fetch_sub(1, Ordering::AcqRel);
                Some(item)
            }
            None => None,
        }
    }

    /// Returns the current number of items in the queue
    pub fn len(&self) -> usize {
        self.size.load(Ordering::Acquire)
    }

    /// Checks if the queue is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clears all items from the queue
    pub fn clear(&self) {
        while self.pop().is_some() {
            // Keep popping until empty
        }
    }

    /// Returns the capacity of the queue
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Checks if the queue is full
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity
    }
}

// Implement Send and Sync for ReadyQueue
unsafe impl<T: Send> Send for ReadyQueue<T> {}
unsafe impl<T: Send> Sync for ReadyQueue<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_basic_operations() {
        let queue = ReadyQueue::new(3);
        
        // Test empty queue
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.pop(), None);

        // Test push
        assert!(queue.push(1));
        assert!(queue.push(2));
        assert!(queue.push(3));
        assert_eq!(queue.len(), 3);
        assert!(!queue.is_empty());

        // Test capacity limit
        assert!(!queue.push(4)); // Should fail due to capacity
        assert!(queue.is_full());

        // Test pop
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.len(), 1);

        // Test push after pop
        assert!(queue.push(4));
        assert_eq!(queue.len(), 2);

        // Test clear
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn test_concurrent_push_pop() {
        let queue = Arc::new(ReadyQueue::new(1000));
        let num_threads = 8;
        let items_per_thread = 100;

        let mut handles = vec![];

        // Spawn producer threads
        for i in 0..num_threads {
            let queue_clone = Arc::clone(&queue);
            let handle = thread::spawn(move || {
                for j in 0..items_per_thread {
                    let value = i * items_per_thread + j;
                    while !queue_clone.push(value) {
                        // Retry on capacity limit
                        thread::yield_now();
                    }
                }
            });
            handles.push(handle);
        }

        // Spawn consumer threads
        for _ in 0..num_threads {
            let queue_clone = Arc::clone(&queue);
            let handle = thread::spawn(move || {
                let mut count = 0;
                while count < items_per_thread {
                    if queue_clone.pop().is_some() {
                        count += 1;
                    } else {
                        thread::yield_now();
                    }
                }
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Queue should be empty after all operations
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn test_capacity_enforcement() {
        let queue = Arc::new(ReadyQueue::new(10));
        let num_threads = 4;

        let mut handles = vec![];

        // Try to push more items than capacity from multiple threads
        for i in 0..num_threads {
            let queue_clone = Arc::clone(&queue);
            let handle = thread::spawn(move || {
                let mut pushed = 0;
                for j in 0..10 {
                    if queue_clone.push(i * 10 + j) {
                        pushed += 1;
                    }
                }
                pushed
            });
            handles.push(handle);
        }

        let mut total_pushed = 0;
        for handle in handles {
            total_pushed += handle.join().unwrap();
        }

        // Total pushed should equal capacity
        assert_eq!(total_pushed, 10);
        assert_eq!(queue.len(), 10);
        assert!(queue.is_full());
    }

    #[test]
    fn test_stress_concurrent_operations() {
        let queue = Arc::new(ReadyQueue::new(50));
        let num_threads = 16;
        let duration = Duration::from_millis(100);

        let mut handles = vec![];

        // Spawn threads that continuously push and pop
        for i in 0..num_threads {
            let queue_clone = Arc::clone(&queue);
            let handle = thread::spawn(move || {
                let start = std::time::Instant::now();
                let mut operations = 0;

                while start.elapsed() < duration {
                    if i % 2 == 0 {
                        // Even threads push
                        if queue_clone.push(i) {
                            operations += 1;
                        }
                    } else {
                        // Odd threads pop
                        if queue_clone.pop().is_some() {
                            operations += 1;
                        }
                    }
                }
                operations
            });
            handles.push(handle);
        }

        let mut total_operations = 0;
        for handle in handles {
            total_operations += handle.join().unwrap();
        }

        // Should have performed many operations without issues
        assert!(total_operations > 0);
        
        // Clear the queue to check final state
        queue.clear();
        assert!(queue.is_empty());
    }

    #[test]
    fn test_clear_concurrent() {
        let queue = Arc::new(ReadyQueue::new(100));
        
        // Fill the queue
        for i in 0..50 {
            queue.push(i);
        }

        let queue_clone = Arc::clone(&queue);
        let clear_thread = thread::spawn(move || {
            queue_clone.clear();
        });

        // Try to push while clearing
        for i in 50..60 {
            let _ = queue.push(i);
        }

        clear_thread.join().unwrap();

        // After clear, we might have some items that were pushed during clear
        // But we should be able to clear again and have empty queue
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
    }
}