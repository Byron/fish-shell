//! Redirect scopes and switch evaluation for native Fish compound statements.
use super::*;

impl Session {
    pub(super) fn eval_statement(
        &mut self,
        statement: &ast::Statement,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let args_or_redirs = match statement {
            ast::Statement::Block(node) => Some(&node.args_or_redirs),
            ast::Statement::Brace(node) => Some(&node.args_or_redirs),
            ast::Statement::If(node) => Some(&node.args_or_redirs),
            ast::Statement::Switch(node) => Some(&node.args_or_redirs),
            ast::Statement::Decorated(_) | ast::Statement::Not(_) => None,
        };
        let Some(args_or_redirs) = args_or_redirs.filter(|args| !args.is_empty()) else {
            return self.eval_statement_body(statement, source, host, output);
        };
        let mut redirects = output.redirects.clone();
        for arg in args_or_redirs {
            match arg {
                ast::ArgumentOrRedirection::Redirection(redir) => {
                    self.append_redirection(redir, source, host, &mut redirects)?;
                }
                ast::ArgumentOrRedirection::Argument(_) => {
                    return Err(Error::new("unexpected argument after a block"));
                }
            }
        }
        // Open once for the entire block. Shared opened descriptions retain the
        // read/write position across its commands and nested redirect scopes.
        let redirects = host.prepare_redirects(&redirects)?;
        let saved = std::mem::replace(&mut output.redirects, redirects);
        let result = self.eval_statement_body(statement, source, host, output);
        output.redirects = saved;
        result
    }

    pub(super) fn append_redirection(
        &mut self,
        redir: &ast::Redirection,
        source: &wstr,
        host: &mut impl Host,
        redirects: &mut Vec<Redirection>,
    ) -> Result<()> {
        let raw = WString::from(text(source, &redir.oper));
        let parsed = PipeOrRedir::try_from(raw.as_utfstr())
            .map_err(|_| Error::new("invalid redirection"))?;
        if !(0..=2).contains(&parsed.fd) {
            return Err(unsupported("redirection of descriptors above 2"));
        }
        let target = self.expand_one(&text(source, &redir.target), host)?;
        let action = match parsed.mode {
            RedirectionMode::Overwrite => RedirectAction::File {
                path: target.into_bytes(),
                mode: OpenMode::Write,
            },
            RedirectionMode::Append => RedirectAction::File {
                path: target.into_bytes(),
                mode: OpenMode::Append,
            },
            RedirectionMode::Input => RedirectAction::File {
                path: target.into_bytes(),
                mode: OpenMode::Read,
            },
            RedirectionMode::Fd if target == "-" => RedirectAction::Close,
            RedirectionMode::Fd => RedirectAction::Dup(
                target
                    .parse()
                    .map_err(|_| Error::new("invalid redirection descriptor"))?,
            ),
            RedirectionMode::TryInput | RedirectionMode::NoClob => {
                return Err(unsupported("try-input and no-clobber redirections"));
            }
        };
        redirects.push(Redirection {
            fd: parsed.fd,
            action,
        });
        if parsed.stderr_merge {
            redirects.push(Redirection {
                fd: 2,
                action: RedirectAction::Dup(parsed.fd),
            });
        }
        Ok(())
    }

    pub(super) fn eval_switch(
        &mut self,
        node: &ast::SwitchStatement,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let mut values = self.expand(&text(source, &node.argument), host)?;
        if values.len() > 1 {
            return Err(Error::new(format!(
                "switch: Expected at most one argument, got {}",
                values.len()
            )));
        }
        let value = values.pop().unwrap_or_default();
        self.scopes.push(BTreeMap::new());
        let result = (|| {
            'cases: for case in &node.cases {
                if self.flow != Flow::Normal || host.cancelled() {
                    break;
                }
                let mut patterns = Vec::new();
                for arg in &case.arguments {
                    match self.expand(&text(source, arg), host) {
                        Ok(expanded) => patterns.extend(expanded),
                        Err(error) => {
                            // Fish reports a failed case expansion and proceeds
                            // to subsequent cases, allowing a default to match.
                            self.status = error.status;
                            output.append(host.write(
                                2,
                                format!("fish: {}\n", error.message).as_bytes(),
                                &output.redirects,
                                output.capture,
                            )?)?;
                            continue 'cases;
                        }
                    }
                }
                if patterns
                    .iter()
                    .any(|pattern| expand::wildcard_match(pattern, &value))
                {
                    return self.eval_jobs(&case.body, source, host, output);
                }
            }
            Ok(())
        })();
        self.scopes.pop();
        result
    }
}
