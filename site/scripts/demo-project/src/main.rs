use forecast::Report;

fn main() {
    let city = std::env::args().nth(1).unwrap_or("Lisbon".into());
    let report = fetch(&city);
    println!("{}", report.summary());
    println!("feels like {:.0}°F", report.fahrenheit());
}

fn fetch(city: &str) -> Report {
    // A real app would call a weather API here.
    let celsius = 21.5;
    let rain_chance = 35;
    Report { city: city.to_string(), celsius, rain_chance }
}
