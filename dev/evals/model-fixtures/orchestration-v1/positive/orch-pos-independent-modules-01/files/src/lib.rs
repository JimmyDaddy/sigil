pub mod quota;
pub mod timeout;

pub fn defaults() -> (u32, u32) {
    (quota::default_quota(), timeout::default_timeout())
}
