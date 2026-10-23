//! Native function properties and execution around the shared declaration parser.
use super::*;
use crate::function_options::{self, Context, ParseError};
use crate::parser_keywords::parser_keywords_is_reserved;
use fish_wgetopt::{ArgType, WGetopter, WOption, wopt};
use fish_widestring::L;
use std::fmt::Write as _;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Function {
    pub(super) body: String,
    #[serde(default)]
    pub(super) origin: Option<String>,
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
    #[serde(default)]
    pub(super) name: String,
    pub(super) local_scope: usize,
    pub(super) visibility_start: usize,
}

impl Function {
    /// Like the POSIX annotated definition, materialize captured values as local
    /// assignments so printing and sourcing a function preserves its snapshot.
    pub(super) fn definition(&self, name: &str) -> String {
        let mut definition = format!("function {}", quote(name));
        for target in &self.wrap_targets {
            write!(definition, " --wraps={}", quote(target)).unwrap();
        }
        if !self.description.is_empty() {
            write!(definition, " --description {}", quote(&self.description)).unwrap();
        }
        if !self.shadow_scope {
            definition.push_str(" --no-scope-shadowing");
        }
        for event in &self.events {
            let (option, name) = event.description.option();
            write!(definition, " {option} {}", quote(name)).unwrap();
        }
        if !self.named_arguments.is_empty() {
            definition.push_str(" --argument-names");
            for argument in &self.named_arguments {
                definition.push(' ');
                definition.push_str(&quote(argument));
            }
        }
        for (name, values) in &self.inherit_vars {
            write!(definition, "\n    set -l {}", quote(name)).unwrap();
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
        if !read_only_ok && read_only_variable(&name) {
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
    pub(super) fn functions_builtin(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        self.refresh_function_paths();
        let wide: Vec<WString> = std::iter::once("functions")
            .chain(args.iter().map(String::as_str))
            .map(WString::from)
            .collect();
        let mut argv: Vec<&wstr> = wide.iter().map(WString::as_utfstr).collect();
        const OPTIONS: &[WOption] = &[
            wopt(L!("query"), ArgType::NoArgument, 'q'),
            wopt(L!("erase"), ArgType::NoArgument, 'e'),
            wopt(L!("names"), ArgType::NoArgument, 'n'),
            wopt(L!("copy"), ArgType::NoArgument, 'c'),
            wopt(L!("help"), ArgType::NoArgument, 'h'),
            wopt(L!("no-details"), ArgType::NoArgument, '\x02'),
        ];
        let mut options = WGetopter::new(L!("qench"), OPTIONS, &mut argv);
        let (mut query, mut erase, mut names_only, mut copy, mut help) =
            (false, false, false, false, false);
        while let Some(option) = options.next_opt() {
            match option {
                'q' => query = true,
                'e' => erase = true,
                'n' => names_only = true,
                'c' => copy = true,
                'h' => help = true,
                // Native definitions already omit the source-location comments
                // suppressed by POSIX --no-details. The shared funcsave helper
                // requests this form for files that will be autoloaded later.
                '\x02' => (),
                _ => {
                    let argument = options.argv[options.wopt_index.saturating_sub(1)];
                    return self.functions_failure(
                        2,
                        &format!("{argument}: invalid functions option"),
                        host,
                        output,
                    );
                }
            }
        }
        let args: Vec<String> = options.argv[options.wopt_index..]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        if help {
            self.write(
                b"functions [-q|-e|-n] [NAME ...]; functions --copy SOURCE DESTINATION\n",
                host,
                output,
            )?;
            self.status = 0;
            return Ok(());
        }
        if [query, erase, names_only, copy]
            .into_iter()
            .filter(|enabled| *enabled)
            .count()
            > 1
        {
            return self.functions_failure(2, "invalid combination of options", host, output);
        }
        if copy {
            return self.copy_function(&args, host, output);
        }
        let names = if args.is_empty() {
            if names_only {
                self.function_names()
            } else {
                self.functions.keys().cloned().collect()
            }
        } else {
            args
        };
        self.status = 0;
        for name in names {
            if names_only {
                self.write(format!("{name}\n").as_bytes(), host, output)?;
                continue;
            }
            if erase {
                self.autoload_tombstones.insert(name.clone());
                self.autoloaded_functions.remove(&name);
                self.status |= i32::from(self.functions.remove(&name).is_none());
            } else {
                self.ensure_function(&name, host, output)?;
                if self.flow == Flow::Exit {
                    return Ok(());
                }
                if let Some(function) = self.functions.get(&name) {
                    if !query {
                        self.write(function.definition(&name).as_bytes(), host, output)?;
                    }
                } else {
                    self.status = 1;
                }
            }
        }
        Ok(())
    }

    fn copy_function(
        &mut self,
        args: &[String],
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        let [source, destination] = args else {
            return self.functions_failure(
                2,
                "expected exactly two names (current function name, and new function name)",
                host,
                output,
            );
        };
        self.ensure_function(source, host, output)?;
        if self.flow == Flow::Exit {
            return Ok(());
        }
        if !self.functions.contains_key(source) {
            return self.functions_failure(
                1,
                &format!("function '{source}' does not exist"),
                host,
                output,
            );
        }
        let destination_wide = WString::from(destination.as_str());
        if !function_options::valid_function_name(&destination_wide)
            || parser_keywords_is_reserved(&destination_wide)
        {
            return self.functions_failure(
                2,
                &format!("illegal function name '{destination}'"),
                host,
                output,
            );
        }
        self.ensure_function(destination, host, output)?;
        if self.flow == Flow::Exit {
            return Ok(());
        }
        if self.functions.contains_key(destination) {
            return self.functions_failure(
                1,
                &format!(
                    "function '{destination}' already exists; cannot create copy of '{source}'"
                ),
                host,
                output,
            );
        }
        // Autoloading the destination may have changed or erased the source.
        let Some(mut function) = self.functions.get(source).cloned() else {
            return self.functions_failure(
                1,
                &format!("function '{source}' does not exist"),
                host,
                output,
            );
        };
        // POSIX copies properties, but event and completion registrations belong
        // to the original name. The new definition is attributed to the copy site.
        function.events.clear();
        function.wrap_targets.clear();
        function.origin.clone_from(&self.source_origin);
        self.autoload_tombstones.remove(destination);
        self.autoloaded_functions.remove(destination);
        self.functions.insert(destination.clone(), function);
        self.status = 0;
        Ok(())
    }

    fn functions_failure(
        &mut self,
        status: i32,
        message: &str,
        host: &mut impl Host,
        output: &mut Output,
    ) -> Result<()> {
        self.status = status;
        self.pipeline = vec![status];
        output.append(host.write(
            2,
            format!("functions: {message}\n").as_bytes(),
            &output.redirects,
            output.capture,
        )?)?;
        Ok(())
    }

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
                origin: self.source_origin.clone(),
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
        name: &str,
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
                self.binding(name, args.get(index).cloned().into_iter().collect(), false),
            );
        }
        for (name, values) in function.inherit_vars {
            overwrite_argv |= name == "argv";
            let variable = self.binding(&name, values, false);
            scope.insert(name, variable);
        }
        if !overwrite_argv {
            scope.insert(
                "argv".into(),
                Variable {
                    values: args.to_vec(),
                    exported: false,
                    pathvar: false,
                },
            );
        }
        self.function_frames.push(FunctionFrame {
            name: name.into(),
            local_scope,
            visibility_start,
        });
        self.scopes.push(scope);
        self.function_depth += 1;
        let saved_loops = std::mem::replace(&mut self.loops, 0);
        let saved_sources = std::mem::replace(&mut self.source_depth, 0);
        let saved_origin = std::mem::replace(&mut self.source_origin, function.origin);
        let result = self.eval_source(&function.body, host, output);
        self.source_origin = saved_origin;
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
