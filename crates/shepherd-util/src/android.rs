//! Android (Waydroid) package-name validation.
//!
//! Shared so the security-critical check has a single source of truth: config
//! validation (`shepherd-config`) and the privileged force-stop helper
//! (`shepherd-waydroid-helper`) must agree exactly, since the package name is
//! spliced into `waydroid app launch <pkg>` / `am force-stop <pkg>`.

/// True if `pkg` is a plausible Android package name: at least two
/// dot-separated segments, each starting with an ASCII letter and otherwise
/// containing only ASCII letters, digits, or underscores — the same shape the
/// Android framework enforces.
///
/// This is deliberately strict: it forbids leading `-` (option injection),
/// whitespace, `/`, and shell metacharacters, so the value is safe to pass as a
/// single argv element to `waydroid`/`am` without any quoting.
pub fn is_valid_android_package(pkg: &str) -> bool {
    let mut segments = 0;
    for segment in pkg.split('.') {
        segments += 1;
        let mut chars = segment.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() => {}
            _ => return false,
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    segments >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_names() {
        assert!(is_valid_android_package("com.android.calculator2"));
        assert!(is_valid_android_package("org.khanacademy.android.kids"));
        assert!(is_valid_android_package("com.mojang.minecraftpe"));
        assert!(is_valid_android_package("a.b"));
        assert!(is_valid_android_package("com.example.my_app"));
    }

    #[test]
    fn rejects_invalid_names() {
        assert!(!is_valid_android_package(""), "empty");
        assert!(!is_valid_android_package("noseparator"), "single segment");
        assert!(!is_valid_android_package("com."), "trailing dot");
        assert!(!is_valid_android_package(".com.app"), "leading dot");
        assert!(!is_valid_android_package("com..app"), "empty segment");
        assert!(
            !is_valid_android_package("com.1app.x"),
            "segment starts with digit"
        );
        assert!(!is_valid_android_package("com.app-name.x"), "hyphen");
        assert!(!is_valid_android_package("com.app name.x"), "space");
        assert!(!is_valid_android_package("-rf"), "leading dash");
        assert!(
            !is_valid_android_package("com.app;rm -rf.x"),
            "shell metacharacters"
        );
    }
}
