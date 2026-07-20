pub fn new_id() -> String {
    ulid::Ulid::new().to_string().to_lowercase()
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
