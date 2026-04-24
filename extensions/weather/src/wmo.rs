// WMO weather interpretation codes used by Open-Meteo.
// Reference: https://open-meteo.com/en/docs (section "Weather variable documentation")

pub fn weather_desc(code: u16) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 => "fog",
        48 => "depositing rime fog",
        51 => "light drizzle",
        53 => "moderate drizzle",
        55 => "dense drizzle",
        56 => "light freezing drizzle",
        57 => "dense freezing drizzle",
        61 => "slight rain",
        63 => "moderate rain",
        65 => "heavy rain",
        66 => "light freezing rain",
        67 => "heavy freezing rain",
        71 => "slight snow",
        73 => "moderate snow",
        75 => "heavy snow",
        77 => "snow grains",
        80 => "slight rain showers",
        81 => "moderate rain showers",
        82 => "violent rain showers",
        85 => "slight snow showers",
        86 => "heavy snow showers",
        95 => "thunderstorm",
        96 => "thunderstorm with slight hail",
        99 => "thunderstorm with heavy hail",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_unknown_codes() {
        assert_eq!(weather_desc(0), "clear sky");
        assert_eq!(weather_desc(3), "overcast");
        assert_eq!(weather_desc(65), "heavy rain");
        assert_eq!(weather_desc(95), "thunderstorm");
        assert_eq!(weather_desc(999), "unknown");
    }
}
