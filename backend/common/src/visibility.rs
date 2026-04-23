/// Values for the `heart_rate_visibility` column on the `users` table.
pub mod values {
    /// User's heart rate is visible only to themselves.
    pub const PRIVATE: &str = "private";

    /// Follow group sharing settings (default).
    pub const GROUP_DEFAULT: &str = "group_default";
}
