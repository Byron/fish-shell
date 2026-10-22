//! Native function properties and execution around the shared declaration parser.
use super::*;
use crate::function_options::{self, Context, ParseError};
use crate::parser_keywords::parser_keywords_is_reserved;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Function {
    pub(super) body: String,
    pub(super) description: String,
    pub(super) named_arguments: Vec<String>,
    pub(super) inherit_vars: BTreeMap<String, Vec<String>>,
    pub(super) shadow_scope: bool,
    pub(super) wrap_targets: Vec<String>,
    #[serde(default)]
    pub(super) events: Vec<events::Registration>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct FunctionFrame {
    pub(super) local_scope: usize,
    pub(super) visibility_start: usize,
}

impl Function {
    /// Like the POSIX annotated definition, materialize captured values as local
    /// assignments so printing and sourcing a function preserves its snapshot.
    pub(super) fn definition(&self, name: &str) -> String {
        let mut definition = format!("function {}", quote(name));
        for target in &self.wrap_targets {
            definition.push_str(&format!(" --wraps={}", quote(target)));
        }
        if !self.description.is_empty() {
            definition.push_str(&format!(" --description {}", quote(&self.description)));
        }
        if !self.shadow_scope {
            definition.push_str(" --no-scope-shadowing");
        }
        for event in &self.events {
            let (option, name) = event.description.option();
            definition.push_str(&format!(" {option} {}", quote(name)));
        }
        if !self.named_arguments.is_empty() {
            definition.push_str(" --argument-names");
            for argument in &self.named_arguments {
                definition.push(' ');
                definition.push_str(&quote(argument));
            }
        }
        for (name, values) in &self.inherit_vars {
            definition.push_str(&format!("\n    set -l {}", quote(name)));
            for value in values {
                definition.push(' ');
                definition.push_str(&quote(value));
            }
        }
        definition.push('\n');
        definition.push_str(&self.body);
        if !self.body.ends_with('\n') {
            definition.push('\n');
        }
        definition.push_str("end\n");
        definition
    }
}

fn quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || "_-./!".contains(character))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

struct NativeContext;

impl Context for NativeContext {
    type Event = events::Description;
    type Error = Error;

    fn validate_variable(&mut self, name: &wstr, read_only_ok: bool) -> Result<()> {
        let name = name.to_string();
        valid_name(&name)?;
        if !read_only_ok
            && matches!(
                name.as_str(),
                "status" | "pipestatus" | "last_pid" | "fish_pid" | "PWD"
            )
        {
            return Err(Error::new(format!(
                "function: variable '{name}' is read-only"
            )));
        }
        Ok(())
    }

    fn event(&mut self, option: char, value: &wstr) -> Result<Self::Event> {
        let name = match option {
            'v' => return Ok(events::Description::Variable(value.to_string())),
            'e' => return Ok(events::Description::Generic(value.to_string())),
            's' => "--on-signal",
            'j' => "--on-job-exit",
            'p' => "--on-process-exit",
            _ => unreachable!("unexpected function event option"),
        };
        Err(unsupported(&format!("function event handlers ({name})")))
    }

    fn error(&mut self, error: ParseError<'_>) -> Error {
        Error::new(match error {
            ParseError::UnexpectedPositional(value) => {
                format!("function: {value}: unexpected positional argument")
            }
            ParseError::MissingArgument(value) => {
                format!("function: {value}: requires an argument")
            }
            ParseError::UnexpectedArgument(value) => {
                format!("function: {value}: does not take an argument")
            }
            ParseError::UnknownOption(value) => format!("function: {value}: unknown option"),
        })
    }
}

impl Session {
    pub(super) fn define_function(
        &mut self,
        header: &ast::FunctionHeader,
        body: String,
        source: &wstr,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let mut arguments = self.expand(&text(source, &header.first_arg), host)?;
        for argument in &header.args {
            arguments.extend(self.expand(&text(source, argument), host)?);
        }
        let arguments: Vec<WString> = arguments.into_iter().map(WString::from).collect();
        let mut argv: Vec<&wstr> = arguments.iter().map(|argument| argument.as_ref()).collect();
        let Some(&name) = argv.first() else {
            return Err(Error::new("function: function name required"));
        };
        if !function_options::valid_function_name(name) {
            return Err(Error::new(format!(
                "function: {name}: invalid function name"
            )));
        }
        if parser_keywords_is_reserved(name) {
            return Err(Error::new(format!(
                "function: {name}: cannot use reserved keyword as function name"
            )));
        }
        let options = function_options::parse(&mut argv, &mut NativeContext)?;
        if options.print_help {
            self.write(
                b"function NAME [-d DESCRIPTION] [-w COMMAND] [-a NAMES...] [-V VARIABLE] [-S] [-v VARIABLE] [-e EVENT]\n",
                host,
                output,
            )?;
            self.status = 0;
            return Ok(());
        }
        let inherit_vars = options
            .inherit_vars
            .into_iter()
            .filter_map(|name| {
                let name = name.to_string();
                Some((name.clone(), self.variable(&name)?.values.clone()))
            })
            .collect();
        let name_string = name.to_string();
        let events = self.register_events(options.events)?;
        self.autoload_tombstones.remove(&name_string);
        if !self.function_autoload.autoload_in_progress(name) {
            self.autoloaded_functions.remove(&name_string);
        }
        self.functions.insert(
            name.to_string(),
            Function {
                body,
                description: options.description.to_string(),
                named_arguments: options
                    .named_arguments
                    .into_iter()
                    .map(|name| name.to_string())
                    .collect(),
                inherit_vars,
                shadow_scope: options.shadow_scope,
                wrap_targets: options
                    .wrap_targets
                    .into_iter()
                    .map(|target| target.to_string())
                    .collect(),
                events,
            },
        );
        self.status = 0;
        Ok(())
    }

    pub(super) fn invoke_function(
        &mut self,
        function: Function,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let local_scope = self.scopes.len();
        let visibility_start = if function.shadow_scope {
            local_scope
        } else {
            self.scope_start()
        };
        let mut scope = BTreeMap::new();
        if function.shadow_scope {
            // Exported caller locals cross the function boundary; ordinary
            // locals remain hidden. An unshadowed call shares its caller scopes.
            for locals in &self.scopes[self.scope_start()..] {
                scope.extend(locals.clone());
            }
            scope.retain(|_, variable: &mut Variable| variable.exported);
        }
        let mut overwrite_argv = false;
        for (index, name) in function.named_arguments.iter().enumerate() {
            overwrite_argv |= name == "argv";
            scope.insert(
                name.clone(),
                Variable {
                    values: args.get(index).cloned().into_iter().collect(),
                    exported: false,
                },
            );
        }
        for (name, values) in function.inherit_vars {
            overwrite_argv |= name == "argv";
            scope.insert(
                name,
                Variable {
                    values,
                    exported: false,
                },
            );
        }
        if !overwrite_argv {
            scope.insert(
                "argv".into(),
                Variable {
                    values: args.to_vec(),
                    exported: false,
                },
            );
        }
        self.function_frames.push(FunctionFrame {
            local_scope,
            visibility_start,
        });
        self.scopes.push(scope);
        self.function_depth += 1;
        let saved_loops = std::mem::replace(&mut self.loops, 0);
        let saved_sources = std::mem::replace(&mut self.source_depth, 0);
        let result = self.eval_source(&function.body, host, output);
        self.source_depth = saved_sources;
        self.loops = saved_loops;
        self.function_depth -= 1;
        self.scopes.pop();
        self.function_frames.pop();
        if self.flow == Flow::Return {
            self.flow = Flow::Normal;
        }
        result
    }
}
