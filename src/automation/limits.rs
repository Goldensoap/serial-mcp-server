use crate::broker::MAX_READ_BYTES;

pub(super) fn max_bytes_error(max_bytes: usize) -> Option<String> {
    if max_bytes == 0 {
        Some("max_bytes must be greater than zero".to_string())
    } else if max_bytes > MAX_READ_BYTES {
        Some(format!("max_bytes must not exceed {MAX_READ_BYTES}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_bytes_bounds_match_the_broker_limit() {
        assert_eq!(
            max_bytes_error(0).as_deref(),
            Some("max_bytes must be greater than zero")
        );
        assert_eq!(max_bytes_error(1), None);
        assert_eq!(max_bytes_error(MAX_READ_BYTES), None);
        assert_eq!(
            max_bytes_error(MAX_READ_BYTES + 1).as_deref(),
            Some("max_bytes must not exceed 65536")
        );
    }
}
