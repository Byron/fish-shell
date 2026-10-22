//! Session state and output adapter for the shared random builtin.
use super::*;
use crate::random_core::{self, Request, Value};
use fish_widestring::L;

impl Session {
    pub(super) fn random_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let owned: Vec<WString> = args.iter().map(|arg| WString::from(arg.as_str())).collect();
        let mut argv: Vec<&wstr> = std::iter::once(L!("random"))
            .chain(owned.iter().map(WString::as_utfstr))
            .collect();
        let request = match random_core::parse(&mut argv) {
            Ok(request) => request,
            Err(error) => {
                self.status = error.status();
                output.append(host.write(
                    2,
                    format!("random: {error}\n").as_bytes(),
                    &output.redirects,
                    output.capture,
                )?)?;
                return Ok(());
            }
        };
        match request {
            Request::Help => self.write(
                b"random [SEED | START [STEP] END | choice ITEMS ...]\n",
                host,
                output,
            )?,
            Request::Seed(seed) => self.random = Some(random_core::seeded(seed)),
            Request::Draw(draw) => {
                let engine = self.random.get_or_insert_with(random_core::fresh);
                let value = match draw.sample(engine) {
                    Value::Number(value) => value.to_string(),
                    Value::Argument(index) => argv[index].to_string(),
                };
                self.write(format!("{value}\n").as_bytes(), host, output)?;
            }
        }
        self.status = 0;
        Ok(())
    }
}
