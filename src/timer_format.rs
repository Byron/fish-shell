//! Shared timing report for the POSIX and native Windows evaluators.
use std::fmt::Write as _;
use std::time::Duration;

enum Unit {
    Minutes,
    Seconds,
    Millis,
    Micros,
}

pub(crate) fn format(
    wall: Duration,
    fish_sys: Duration,
    fish_usr: Duration,
    child_sys: Duration,
    child_usr: Duration,
    verbose: bool,
) -> String {
    let net_wall_micros = wall.as_micros() as i64;
    let net_sys_micros = (fish_sys + child_sys).as_micros() as i64;
    let net_usr_micros = (fish_usr + child_usr).as_micros() as i64;

    let wall_unit = Unit::for_micros(net_wall_micros);
    // Make sure we share the same unit for the various CPU times
    let cpu_unit = Unit::for_micros(net_sys_micros.max(net_usr_micros));

    let wall_time = wall_unit.convert_micros(net_wall_micros);
    let sys_time = cpu_unit.convert_micros(net_sys_micros);
    let usr_time = cpu_unit.convert_micros(net_usr_micros);

    let mut output = String::new();
    #[rustfmt::skip]
    if !verbose {
        write!(output, "\n_______________________________").unwrap();
        write!(output, "\nExecuted in  {:6.2} {}", wall_time, wall_unit.long_name()).unwrap();
        write!(output, "\n   usr time  {:6.2} {}", usr_time, cpu_unit.long_name()).unwrap();
        write!(output, "\n   sys time  {:6.2} {}", sys_time, cpu_unit.long_name()).unwrap();
    } else {
        let fish_unit = Unit::for_micros(fish_sys.max(fish_usr).as_micros() as i64);
        let child_unit = Unit::for_micros(child_sys.max(child_usr).as_micros() as i64);
        let fish_usr_time = fish_unit.convert_micros(fish_usr.as_micros() as i64);
        let fish_sys_time = fish_unit.convert_micros(fish_sys.as_micros() as i64);
        let child_usr_time = child_unit.convert_micros(child_usr.as_micros() as i64);
        let child_sys_time = child_unit.convert_micros(child_sys.as_micros() as i64);

        let column2_unit_len = wall_unit
            .short_name()
            .len()
            .max(cpu_unit.short_name().len());
        let wall_unit = wall_unit.short_name();
        let cpu_unit = cpu_unit.short_name();
        let fish_unit = fish_unit.short_name();
        let child_unit = child_unit.short_name();

        output += "\n________________________________________________________";
        write!(
            output,
            "\nExecuted in  {wall_time:6.2} {wall_unit:<width1$}    {fish:<width2$}  external",
            width1 = column2_unit_len,
            fish = "fish",
            width2 = fish_unit.len() + 7
        )
        .unwrap();
        write!(output, "\n   usr time  {usr_time:6.2} {cpu_unit:<column2_unit_len$}  {fish_usr_time:6.2} {fish_unit}  {child_usr_time:6.2} {child_unit}").unwrap();
        write!(output, "\n   sys time  {sys_time:6.2} {cpu_unit:<column2_unit_len$}  {fish_sys_time:6.2} {fish_unit}  {child_sys_time:6.2} {child_unit}").unwrap();
    };
    output += "\n";

    output
}

impl Unit {
    /// Return the appropriate unit to format the provided number of microseconds in.
    const fn for_micros(micros: i64) -> Unit {
        match micros {
            900_000_001.. => Unit::Minutes,
            // Move to seconds if we would overflow the %6.2 format
            999_995.. => Unit::Seconds,
            1000.. => Unit::Millis,
            _ => Unit::Micros,
        }
    }

    const fn short_name(&self) -> &'static str {
        match *self {
            Unit::Minutes => "mins",
            Unit::Seconds => "secs",
            Unit::Millis => "millis",
            Unit::Micros => "micros",
        }
    }

    const fn long_name(&self) -> &'static str {
        match *self {
            Unit::Minutes => "minutes",
            Unit::Seconds => "seconds",
            Unit::Millis => "milliseconds",
            Unit::Micros => "microseconds",
        }
    }

    fn convert_micros(&self, micros: i64) -> f64 {
        match *self {
            Unit::Minutes => micros as f64 / 1.0E6 / 60.0,
            Unit::Seconds => micros as f64 / 1.0E6,
            Unit::Millis => micros as f64 / 1.0E3,
            Unit::Micros => micros as f64 / 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_format_and_alignment() {
        let actual = format(
            Duration::from_micros(500),
            Duration::from_micros(999994),
            Duration::from_micros(999995),
            Duration::from_micros(500),
            Duration::from_micros(1000),
            true,
        );
        assert_eq!(
            actual,
            "\n________________________________________________________\nExecuted in  500.00 micros    fish         external\n   usr time    1.00 secs      1.00 secs    1.00 millis\n   sys time    1.00 secs      1.00 secs    0.50 millis\n"
        );
    }
}
