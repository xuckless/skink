/// One day of weather for a city.
pub struct Report {
    pub city: String,
    pub celsius: f64,
    pub rain_chance: u8,
}

impl Report {
    pub fn fahrenheit(&self) -> f64 {
        self.celsius * 9.0 / 5.0 + 32.0
    }

    pub fn summary(&self) -> String {
        let sky = if self.rain_chance > 60 { "rain" } else { "clear" };
        format!("{}: {:.0}°C, {}", self.city, self.celsius, sky)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts() {
        let r = Report { city: "Oslo".into(), celsius: 10.0, rain_chance: 20 };
        assert_eq!(r.fahrenheit(), 50.0);
    }
}
