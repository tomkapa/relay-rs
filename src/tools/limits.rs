use std::time::Duration;

pub const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
pub const FETCH_MAX_BODY_BYTES: usize = 200_000;

pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
pub const SEARCH_DEFAULT_COUNT: u8 = 5;
pub const SEARCH_MAX_COUNT: u8 = 10;
