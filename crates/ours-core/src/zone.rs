//! Time zones from the bundled IANA database, so the engine never reads the
//! host's zone files, and each member's zone (DESIGN.md section 12,
//! "Everyone's clock").

use crate::error::CoreError;
use crate::state::GroupState;
use jiff::Zoned;
use jiff::tz::{TimeZone, TimeZoneDatabase};
use ours_types::{Instant, Member, PlatformUserId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How sure the engine is of a zone the platform reported.
pub const PLATFORM_CONFIDENCE: f32 = 0.8;
/// How sure it is of a zone the member stated.
pub const STATED_CONFIDENCE: f32 = 0.9;

/// A member's zone and how it is known.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Zone {
    /// An IANA name such as "Europe/Lisbon".
    pub name: String,
    pub confidence: f32,
}

pub(crate) fn lookup(name: &str) -> Result<TimeZone, CoreError> {
    TimeZoneDatabase::bundled()
        .get(name)
        .map_err(|_| CoreError::UnknownTimezone(name.to_owned()))
}

/// Whether `name` is a zone the engine can use, such as "Europe/London".
pub fn is_known(name: &str) -> bool {
    lookup(name).is_ok()
}

/// The named zone, or UTC when the name is unknown.
pub(crate) fn or_utc(name: &str) -> TimeZone {
    lookup(name).unwrap_or(TimeZone::UTC)
}

/// `now` as a civil time in the named zone.
pub(crate) fn local(now: Instant, zone_name: &str) -> Zoned {
    now.0.to_zoned(or_utc(zone_name))
}

/// `at` as "2026-11-02 18:00" in the named zone.
pub(crate) fn stamp(at: Instant, zone_name: &str) -> String {
    local(at, zone_name).strftime("%Y-%m-%d %H:%M").to_string()
}

/// The zone a member's times are read in: their own when known, else the
/// group's.
pub fn zone_of(state: &GroupState, user: &PlatformUserId) -> String {
    state
        .zones
        .get(user)
        .map_or_else(|| state.timezone.clone(), |z| z.name.clone())
}

/// Takes a member's zone from the platform, unless they stated one.
pub(crate) fn from_platform(state: &mut GroupState, member: &Member) {
    let Some(name) = member.timezone.as_deref().and_then(canonical) else {
        return;
    };
    let stated = state
        .zones
        .get(&member.id)
        .is_some_and(|z| z.confidence > PLATFORM_CONFIDENCE);
    if !stated {
        state.zones.insert(
            member.id.clone(),
            Zone {
                name,
                confidence: PLATFORM_CONFIDENCE,
            },
        );
    }
}

/// The database's own spelling of a zone name given in any case.
pub fn canonical(name: &str) -> Option<String> {
    let tz = lookup(name.trim()).ok()?;
    tz.iana_name().map(str::to_owned)
}

/// Whether dates like 3/4 mean March 4 in this zone.
pub fn month_first(zone_name: &str) -> bool {
    zone_name.starts_with("US/")
        || matches!(
            zone_name,
            "America/New_York"
                | "America/Chicago"
                | "America/Denver"
                | "America/Phoenix"
                | "America/Los_Angeles"
                | "America/Anchorage"
                | "America/Detroit"
                | "America/Boise"
                | "America/Indiana/Indianapolis"
                | "America/Kentucky/Louisville"
                | "America/Puerto_Rico"
                | "Pacific/Honolulu"
        )
}

/// Cities people name when they say where they are.
const CITIES: [(&str, &str); 92] = [
    ("london", "Europe/London"),
    ("manchester", "Europe/London"),
    ("edinburgh", "Europe/London"),
    ("glasgow", "Europe/London"),
    ("leith", "Europe/London"),
    ("bristol", "Europe/London"),
    ("dublin", "Europe/Dublin"),
    ("lisbon", "Europe/Lisbon"),
    ("porto", "Europe/Lisbon"),
    ("madrid", "Europe/Madrid"),
    ("barcelona", "Europe/Madrid"),
    ("paris", "Europe/Paris"),
    ("brussels", "Europe/Brussels"),
    ("amsterdam", "Europe/Amsterdam"),
    ("berlin", "Europe/Berlin"),
    ("munich", "Europe/Berlin"),
    ("hamburg", "Europe/Berlin"),
    ("zurich", "Europe/Zurich"),
    ("geneva", "Europe/Zurich"),
    ("vienna", "Europe/Vienna"),
    ("rome", "Europe/Rome"),
    ("milan", "Europe/Rome"),
    ("prague", "Europe/Prague"),
    ("warsaw", "Europe/Warsaw"),
    ("copenhagen", "Europe/Copenhagen"),
    ("stockholm", "Europe/Stockholm"),
    ("oslo", "Europe/Oslo"),
    ("helsinki", "Europe/Helsinki"),
    ("athens", "Europe/Athens"),
    ("istanbul", "Europe/Istanbul"),
    ("moscow", "Europe/Moscow"),
    ("kyiv", "Europe/Kyiv"),
    ("reykjavik", "Atlantic/Reykjavik"),
    ("new york", "America/New_York"),
    ("nyc", "America/New_York"),
    ("boston", "America/New_York"),
    ("washington", "America/New_York"),
    ("miami", "America/New_York"),
    ("atlanta", "America/New_York"),
    ("toronto", "America/Toronto"),
    ("montreal", "America/Toronto"),
    ("chicago", "America/Chicago"),
    ("austin", "America/Chicago"),
    ("dallas", "America/Chicago"),
    ("houston", "America/Chicago"),
    ("denver", "America/Denver"),
    ("phoenix", "America/Phoenix"),
    ("los angeles", "America/Los_Angeles"),
    ("la", "America/Los_Angeles"),
    ("san francisco", "America/Los_Angeles"),
    ("sf", "America/Los_Angeles"),
    ("seattle", "America/Los_Angeles"),
    ("portland", "America/Los_Angeles"),
    ("vancouver", "America/Vancouver"),
    ("mexico city", "America/Mexico_City"),
    ("sao paulo", "America/Sao_Paulo"),
    ("são paulo", "America/Sao_Paulo"),
    ("buenos aires", "America/Argentina/Buenos_Aires"),
    ("honolulu", "Pacific/Honolulu"),
    ("tokyo", "Asia/Tokyo"),
    ("seoul", "Asia/Seoul"),
    ("beijing", "Asia/Shanghai"),
    ("shanghai", "Asia/Shanghai"),
    ("hong kong", "Asia/Hong_Kong"),
    ("taipei", "Asia/Taipei"),
    ("singapore", "Asia/Singapore"),
    ("bangkok", "Asia/Bangkok"),
    ("jakarta", "Asia/Jakarta"),
    ("manila", "Asia/Manila"),
    ("kuala lumpur", "Asia/Kuala_Lumpur"),
    ("mumbai", "Asia/Kolkata"),
    ("delhi", "Asia/Kolkata"),
    ("new delhi", "Asia/Kolkata"),
    ("bangalore", "Asia/Kolkata"),
    ("bengaluru", "Asia/Kolkata"),
    ("dubai", "Asia/Dubai"),
    ("tel aviv", "Asia/Jerusalem"),
    ("sydney", "Australia/Sydney"),
    ("canberra", "Australia/Sydney"),
    ("melbourne", "Australia/Melbourne"),
    ("brisbane", "Australia/Brisbane"),
    ("perth", "Australia/Perth"),
    ("adelaide", "Australia/Adelaide"),
    ("hobart", "Australia/Hobart"),
    ("darwin", "Australia/Darwin"),
    ("auckland", "Pacific/Auckland"),
    ("wellington", "Pacific/Auckland"),
    ("cairo", "Africa/Cairo"),
    ("lagos", "Africa/Lagos"),
    ("nairobi", "Africa/Nairobi"),
    ("johannesburg", "Africa/Johannesburg"),
    ("cape town", "Africa/Johannesburg"),
];

/// The zone a place names: an IANA name in any case, or a city from the
/// table. "Lisbon" and "europe/lisbon" both give "Europe/Lisbon".
pub fn named_zone(place: &str) -> Option<String> {
    let place = place
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .trim()
        .to_lowercase();
    if place.contains('/') || place == "utc" {
        return canonical(&place);
    }
    CITIES
        .iter()
        .find(|(city, _)| *city == place)
        .map(|(_, zone)| (*zone).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_in_any_case() {
        assert_eq!(canonical("europe/lisbon").as_deref(), Some("Europe/Lisbon"));
        assert_eq!(named_zone("Lisbon").as_deref(), Some("Europe/Lisbon"));
        assert_eq!(
            named_zone("America/New_York").as_deref(),
            Some("America/New_York")
        );
        assert_eq!(named_zone("new york").as_deref(), Some("America/New_York"));
        assert!(named_zone("Narnia").is_none());
        assert!(named_zone("Mars/Olympus").is_none());
        for (_, zone) in CITIES {
            assert!(is_known(zone), "{zone}");
        }
        assert!(month_first("America/Chicago"));
        assert!(!month_first("Europe/London"));
    }
}
