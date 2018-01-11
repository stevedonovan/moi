use std::time::{Instant,Duration};
use std::sync::{Mutex,Arc};

pub struct Timeout {
    last_msg_time: Instant,
    timeout: Duration,
}

impl Timeout {
    pub fn new (timeout: i32) -> Timeout {
        Timeout {
            last_msg_time: Instant::now(),
            timeout: Duration::from_millis(timeout as u64),
        }
    }

    pub fn new_shared(timeout: i32) -> Arc<Mutex<Timeout>> {
        Arc::new(Mutex::new(Timeout::new(timeout)))
    }

    pub fn set_timeout(&mut self, timeout: i32) {
        self.timeout = Duration::from_millis(timeout as u64)
    }

    pub fn update(&mut self) {
        self.last_msg_time = Instant::now();
    }

    pub fn timed_out(&self) -> bool {
        self.last_msg_time.elapsed() > self.timeout
    }

}
