/// Build script that embeds a compile timestamp as an environment variable.
/// This gives every build a unique identifier for verifying deployment.
fn main() {
    // Embed compile timestamp so we can verify which build is running.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Format as a readable timestamp (UTC)
    let secs_per_min = 60u64;
    let secs_per_hour = 3600u64;
    let secs_per_day = 86400u64;

    // Simple date/time calculation (good enough for a build stamp)
    let days = now / secs_per_day;
    let time_of_day = now % secs_per_day;
    let hour = time_of_day / secs_per_hour;
    let minute = (time_of_day % secs_per_hour) / secs_per_min;
    let second = time_of_day % secs_per_min;

    // Days since epoch to Y-M-D (simplified)
    let (year, month, day) = days_to_ymd(days);

    let build_id = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z ({})",
        year, month, day, hour, minute, second, now
    );

    println!("cargo:rustc-env=SPAMBAYES_BUILD_ID={build_id}");
    // Always re-run so the timestamp updates on every build
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=FORCE_REBUILD");
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Compute year/month/day from days since Unix epoch (1970-01-01)
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];

    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, days + 1)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}
