use std::time::{Instant,Duration};
use super::*;

pub struct Timeout {
    last_msg_time: Instant,
    timeout: Duration,
    millis: i32,
    enabled: bool,
}

impl Timeout {
    pub fn new (timeout: i32) -> Timeout {
        Timeout {
            last_msg_time: Instant::now(),
            timeout: Duration::from_millis(timeout as u64),
            millis: timeout,
            enabled: true,
        }
    }

    pub fn new_shared(timeout: i32) -> SharedPtr<Timeout> {
        make_shared(Timeout::new(timeout))
    }

    pub fn set_timeout(&mut self, timeout: i32) {
        self.timeout = Duration::from_millis(timeout as u64)
    }

    pub fn disable(&mut self) {
        self.enabled = false;
    }

    pub fn enable(&mut self) {
        self.enabled = true;
        self.update();
        let millis = self.millis;
        self.set_timeout(millis);
    }

    pub fn update(&mut self) {
        self.last_msg_time = Instant::now();
    }

    pub fn timed_out(&self) -> bool {
        self.enabled && self.last_msg_time.elapsed() > self.timeout
    }

}
