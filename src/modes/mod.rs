pub mod dealer_mode;
pub mod pub_mode;
pub mod router_mode;
pub mod sub_mode;

#[cfg(test)]
mod roundtrip_tests;

#[cfg(test)]
pub(crate) mod test_util {
    use std::path::PathBuf;

    /// A uniquely named file in the OS temp dir that is removed on drop.
    pub(crate) struct TempPath(PathBuf);

    impl TempPath {
        /// Reserves a unique path without creating the file.
        pub(crate) fn new(name: &str) -> Self {
            let unique = format!("zmqc_test_{}_{}", std::process::id(), name);
            let path = std::env::temp_dir().join(unique);
            let _ = std::fs::remove_file(&path);
            Self(path)
        }

        pub(crate) fn with_bytes(name: &str, bytes: &[u8]) -> Self {
            let p = Self::new(name);
            std::fs::write(&p.0, bytes).unwrap();
            p
        }

        pub(crate) fn as_str(&self) -> &str {
            self.0.to_str().unwrap()
        }

        pub(crate) fn read_to_string(&self) -> String {
            std::fs::read_to_string(&self.0).unwrap()
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}
