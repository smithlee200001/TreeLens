pub mod footprint;
pub mod scanner;
pub mod session;
pub mod tree;

/// Binary units: the app never labels powers of 1024 as decimal MB/GB.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let exponent = ((63 - bytes.leading_zeros()) / 10).min(6) as usize;
    let value = bytes as f64 / 1024_f64.powi(exponent as i32);
    format!("{value:.2} {}", UNITS[exponent])
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_bytes_without_overflow() {
        assert_eq!(super::format_bytes(0), "0 B");
        assert_eq!(super::format_bytes(1023), "1023 B");
        assert_eq!(super::format_bytes(1024), "1.00 KiB");
        assert_eq!(super::format_bytes(1 << 30), "1.00 GiB");
        assert_eq!(super::format_bytes(u64::MAX), "16.00 EiB");
    }
}
