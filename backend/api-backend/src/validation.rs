use std::fmt::Display;

use crate::auth::AppError;

const MAX_NAME_CHARS: usize = 50;
const MAX_TIMEZONE_BYTES: usize = 64;

pub fn validate_required_name(raw: &str, field: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(format!("{field} cannot be empty")));
    }
    check_name_shape(trimmed, field)?;
    Ok(trimmed.to_string())
}

pub fn validate_optional_name(raw: &str, field: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if !trimmed.is_empty() {
        check_name_shape(trimmed, field)?;
    }
    Ok(trimmed.to_string())
}

fn check_name_shape(s: &str, field: &str) -> Result<(), AppError> {
    if s.chars().count() > MAX_NAME_CHARS {
        return Err(AppError::BadRequest(format!(
            "{field} must be {MAX_NAME_CHARS} characters or fewer"
        )));
    }
    if s.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!(
            "{field} cannot contain control characters"
        )));
    }
    Ok(())
}

pub fn validate_timezone(raw: &str) -> Result<&str, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("timezone cannot be empty".into()));
    }
    if trimmed.len() > MAX_TIMEZONE_BYTES {
        return Err(AppError::BadRequest("timezone name too long".into()));
    }
    trimmed
        .parse::<chrono_tz::Tz>()
        .map_err(|_| AppError::BadRequest(format!("Unknown timezone: {trimmed}")))?;
    Ok(trimmed)
}

pub fn validate_range<T>(v: T, min: T, max: T, field: &str) -> Result<T, AppError>
where
    T: Ord + Copy + Display,
{
    if v < min || v > max {
        return Err(AppError::BadRequest(format!(
            "{field} must be between {min} and {max}"
        )));
    }
    Ok(v)
}

pub fn validate_secret<'a>(raw: &'a str, max: usize, field: &str) -> Result<&'a str, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(format!("{field} cannot be empty")));
    }
    if trimmed.len() > max {
        return Err(AppError::BadRequest(format!("{field} too long")));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!(
            "{field} cannot contain control characters"
        )));
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_name_rejects_empty_and_whitespace() {
        assert!(validate_required_name("", "display_name").is_err());
        assert!(validate_required_name("   ", "display_name").is_err());
    }

    #[test]
    fn required_name_trims() {
        assert_eq!(
            validate_required_name("  Alice  ", "display_name").unwrap(),
            "Alice"
        );
    }

    #[test]
    fn required_name_accepts_non_ascii() {
        assert_eq!(
            validate_required_name("山田太郎", "display_name").unwrap(),
            "山田太郎"
        );
    }

    #[test]
    fn required_name_counts_codepoints_not_bytes() {
        let fifty = "あ".repeat(50);
        assert_eq!(
            validate_required_name(&fifty, "display_name").unwrap(),
            fifty
        );
        let fifty_one = "あ".repeat(51);
        assert!(validate_required_name(&fifty_one, "display_name").is_err());
    }

    #[test]
    fn required_name_boundary() {
        let fifty = "a".repeat(50);
        assert!(validate_required_name(&fifty, "display_name").is_ok());
        let fifty_one = "a".repeat(51);
        assert!(validate_required_name(&fifty_one, "display_name").is_err());
    }

    #[test]
    fn required_name_rejects_control_chars() {
        assert!(validate_required_name("hello\nworld", "display_name").is_err());
        assert!(validate_required_name("hello\0world", "display_name").is_err());
        assert!(validate_required_name("hello\tworld", "display_name").is_err());
    }

    #[test]
    fn optional_name_allows_empty() {
        assert_eq!(validate_optional_name("", "name").unwrap(), "");
        assert_eq!(validate_optional_name("   ", "name").unwrap(), "");
    }

    #[test]
    fn optional_name_trims_non_empty() {
        assert_eq!(validate_optional_name("  team  ", "name").unwrap(), "team");
    }

    #[test]
    fn optional_name_enforces_length() {
        let too_long = "a".repeat(51);
        assert!(validate_optional_name(&too_long, "name").is_err());
    }

    #[test]
    fn optional_name_rejects_control_chars() {
        assert!(validate_optional_name("my\nteam", "name").is_err());
    }

    #[test]
    fn timezone_accepts_common_iana() {
        assert!(validate_timezone("UTC").is_ok());
        assert!(validate_timezone("Asia/Tokyo").is_ok());
        assert!(validate_timezone("America/New_York").is_ok());
        assert!(validate_timezone("America/Argentina/ComodRivadavia").is_ok());
    }

    #[test]
    fn timezone_rejects_empty() {
        assert!(validate_timezone("").is_err());
        assert!(validate_timezone("   ").is_err());
    }

    #[test]
    fn timezone_rejects_bogus() {
        assert!(validate_timezone("NotATimezone").is_err());
        assert!(validate_timezone("Asia/Nowhere").is_err());
        assert!(validate_timezone("asia/tokyo").is_err());
        assert!(validate_timezone("GMT+9").is_err());
    }

    #[test]
    fn timezone_rejects_overlong() {
        let s = "A".repeat(65);
        assert!(validate_timezone(&s).is_err());
    }

    #[test]
    fn range_boundary() {
        assert!(validate_range(1i64, 1, 10, "v").is_ok());
        assert!(validate_range(10i64, 1, 10, "v").is_ok());
        assert!(validate_range(0i64, 1, 10, "v").is_err());
        assert!(validate_range(11i64, 1, 10, "v").is_err());
    }

    #[test]
    fn secret_rejects_empty() {
        assert!(validate_secret("", 4096, "access_token").is_err());
        assert!(validate_secret("   ", 4096, "access_token").is_err());
    }

    #[test]
    fn secret_rejects_overlong() {
        let s = "a".repeat(4097);
        assert!(validate_secret(&s, 4096, "access_token").is_err());
    }

    #[test]
    fn secret_boundary() {
        let s = "a".repeat(4096);
        assert!(validate_secret(&s, 4096, "access_token").is_ok());
    }

    #[test]
    fn secret_rejects_control_chars() {
        assert!(validate_secret("abc\tdef", 4096, "access_token").is_err());
        assert!(validate_secret("abc\0def", 4096, "access_token").is_err());
    }

    #[test]
    fn secret_trims() {
        assert_eq!(
            validate_secret("  token  ", 4096, "access_token").unwrap(),
            "token"
        );
    }
}
