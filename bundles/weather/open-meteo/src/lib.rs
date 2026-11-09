//! The Open-Meteo forecast API (https://open-meteo.com, free, no API key): the current
//! weather and the next 24 hours, for the weather bundle and the dashboard.
//!
//! HTTPS with hyper over tokio-openssl over BoringSSL, the stack SDV uses for SDV-RPC.

use std::error::Error;
use std::fmt;

const HOST: &str = "api.open-meteo.com";
/// The forecast is a few kilobytes.
const MAX_BODY: usize = 256 * 1024;

/// Where to ask for the weather.
pub struct Location {
    /// The place, as the dashboard shows it.
    pub name: &'static str,
    /// Degrees north.
    pub latitude: f64,
    /// Degrees east.
    pub longitude: f64,
}

/// Berlin; one fixed location until the house has a settings store.
pub const BERLIN: Location = Location { name: "Berlin", latitude: 52.52, longitude: 13.40 };

/// The current weather at a location and the next 24 hours.
#[derive(Debug, Clone, PartialEq)]
pub struct Weather {
    /// Local time of the reading, "HH:MM".
    pub time: String,
    /// Air temperature 2 m above ground, °C.
    pub temperature_c: f64,
    /// Apparent ("feels like") temperature, °C.
    pub apparent_c: f64,
    /// Relative humidity 2 m above ground, %.
    pub humidity_percent: f64,
    /// Wind speed 10 m above ground, km/h.
    pub wind_kmh: f64,
    /// WMO weather interpretation code.
    pub code: u32,
    /// The current hour and the 23 after it.
    pub hourly: Vec<Hour>,
}

/// One hour of the forecast.
#[derive(Debug, Clone, PartialEq)]
pub struct Hour {
    /// Local hour of the day, 0..23.
    pub hour: u8,
    /// Air temperature, °C.
    pub temperature_c: f64,
    /// Probability of precipitation, %.
    pub precipitation_percent: f64,
    /// WMO weather interpretation code.
    pub code: u32,
}

impl Weather {
    /// The WMO code as words, as on the dashboard tile.
    pub fn condition(&self) -> &'static str {
        match self.code {
            0 => "Clear",
            1 => "Mainly clear",
            2 => "Partly cloudy",
            3 => "Overcast",
            45 | 48 => "Fog",
            51 | 53 | 55 => "Drizzle",
            56 | 57 => "Freezing drizzle",
            61 | 63 | 65 => "Rain",
            66 | 67 => "Freezing rain",
            71 | 73 | 75 | 77 => "Snow",
            80..=82 => "Rain showers",
            85 | 86 => "Snow showers",
            95 => "Thunderstorm",
            96 | 99 => "Thunderstorm with hail",
            _ => "Unknown",
        }
    }
}

impl fmt::Display for Weather {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:.1} °C, {:.0} %, {} (code {})",
            self.temperature_c,
            self.humidity_percent,
            self.condition(),
            self.code
        )
    }
}

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Fetches the current weather and the hourly forecast (today and tomorrow, local time).
pub async fn fetch(location: &Location) -> Result<Weather> {
    let path = format!(
        "/v1/forecast?latitude={}&longitude={}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,wind_speed_10m,weather_code\
         &hourly=temperature_2m,precipitation_probability,weather_code&forecast_days=2&timezone=auto",
        location.latitude, location.longitude
    );
    let body = everyware_https::get_text(&format!("https://{HOST}{path}"), MAX_BODY).await?;
    parse(&body)
}

/// The hours the dashboard shows.
pub const HOURS: usize = 24;

fn parse(body: &str) -> Result<Weather> {
    let json: serde_json::Value = serde_json::from_str(body)?;
    let current = &json["current"];
    let number = |key: &str| current[key].as_f64().ok_or_else(|| format!("no current.{key}"));
    // "2026-11-05T13:45": the hour of the reading starts the hourly list
    let time = current["time"].as_str().ok_or("no current.time")?;
    let hourly = &json["hourly"];
    let times = hourly["time"].as_array().ok_or("no hourly.time")?;
    let first = times
        .iter()
        .position(|t| t.as_str().is_some_and(|t| t.len() >= 13 && time.len() >= 13 && t[..13] == time[..13]))
        .ok_or("current hour not in hourly.time")?;
    let series = |key: &str| hourly[key].as_array().ok_or_else(|| format!("no hourly.{key}"));
    let (temps, rain, codes) = (series("temperature_2m")?, series("precipitation_probability")?, series("weather_code")?);
    let hours = (first..(first + HOURS).min(times.len()))
        .map(|i| {
            let value = |a: &[serde_json::Value]| a.get(i).and_then(|v| v.as_f64()).ok_or("hourly value missing");
            Ok(Hour {
                hour: times[i].as_str().and_then(|t| t.get(11..13)).and_then(|h| h.parse().ok()).ok_or("hourly time")?,
                temperature_c: value(temps)?,
                precipitation_percent: value(rain)?,
                code: value(codes)? as u32,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Weather {
        time: time.get(11..16).unwrap_or("").to_string(),
        temperature_c: number("temperature_2m")?,
        apparent_c: number("apparent_temperature")?,
        humidity_percent: number("relative_humidity_2m")?,
        wind_kmh: number("wind_speed_10m")?,
        code: number("weather_code")? as u32,
        hourly: hours,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_and_the_hours_from_now() {
        let body = r#"{"latitude":52.52,"current":{"time":"2026-11-03T11:45","interval":900,
            "temperature_2m":9.4,"apparent_temperature":7.1,"relative_humidity_2m":81,"wind_speed_10m":14.3,"weather_code":2},
            "hourly":{"time":["2026-11-03T10:00","2026-11-03T11:00","2026-11-03T12:00"],
            "temperature_2m":[8.0,9.0,10.5],"precipitation_probability":[0,20,65],"weather_code":[1,2,61]}}"#;
        let weather = parse(body).unwrap();
        assert_eq!((weather.time.as_str(), weather.temperature_c, weather.apparent_c, weather.wind_kmh), ("11:45", 9.4, 7.1, 14.3));
        assert_eq!(weather.to_string(), "9.4 °C, 81 %, Partly cloudy (code 2)");
        assert_eq!(
            weather.hourly,
            vec![
                Hour { hour: 11, temperature_c: 9.0, precipitation_percent: 20.0, code: 2 },
                Hour { hour: 12, temperature_c: 10.5, precipitation_percent: 65.0, code: 61 },
            ]
        );
    }

    #[test]
    fn an_hourly_list_without_the_current_hour_is_an_error() {
        let body = r#"{"current":{"time":"2026-11-03T11:45","temperature_2m":9.4,"apparent_temperature":7.1,
            "relative_humidity_2m":81,"wind_speed_10m":14.3,"weather_code":2},
            "hourly":{"time":["2026-11-03T10:00"],"temperature_2m":[8.0],"precipitation_probability":[0],"weather_code":[1]}}"#;
        assert!(parse(body).is_err());
    }
}
