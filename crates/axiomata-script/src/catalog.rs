//! What a script can find out about the host's tools: their specs, searched and described from inside the script, and
//! declared to the model as TypeScript-like lines.

use serde_json::{Value, json};

/// Most hits `searchTools` returns.
const MAX_SEARCH_HITS: usize = 20;

/// One tool the host offers a script: its name, what it does, and its arguments as a JSON schema.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// The name a script calls it by, `tools.<name>(...)`.
    pub name: String,
    /// What it does; the first sentence is what searches and declarations show.
    pub description: String,
    /// Its arguments as a JSON schema (an object with `properties` and `required`).
    pub parameters: Value,
}

/// Every tool a script may call, as the host offers them.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// The tools, in the order they are declared.
    pub specs: Vec<ToolSpec>,
}

impl Catalog {
    /// A catalog of these tools.
    #[must_use]
    pub fn new(specs: Vec<ToolSpec>) -> Self {
        Self { specs }
    }

    /// The tools whose name and description hold every word of `query`, ignoring case.
    pub(crate) fn search(&self, query: &str) -> Value {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let hits: Vec<Value> = self
            .specs
            .iter()
            .filter(|spec| {
                let text = format!("{} {}", spec.name, spec.description).to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .take(MAX_SEARCH_HITS)
            .map(|spec| json!({ "name": spec.name, "description": first_sentence(&spec.description) }))
            .collect();
        Value::Array(hits)
    }

    /// One tool with its whole description and its arguments, or `null`.
    pub(crate) fn describe(&self, name: &str) -> Value {
        self.specs.iter().find(|spec| spec.name == name).map_or(Value::Null, |spec| {
            json!({ "name": spec.name, "description": spec.description, "parameters": spec.parameters })
        })
    }

    /// Every tool's name.
    pub(crate) fn names(&self) -> Value {
        Value::Array(
            self.specs
                .iter()
                .map(|spec| Value::String(spec.name.clone()))
                .collect(),
        )
    }
}

/// `tools.read_file(args: {path: string, offset?: number}): Promise<string> // first sentence` per tool, as long as
/// they fit into `budget` bytes; past it only the names, found with `describeTool()`.
#[must_use]
pub fn declarations(specs: &[ToolSpec], budget: usize) -> String {
    let mut out = String::new();
    let mut rest = Vec::new();
    for spec in specs {
        let line = format!(
            "{}(args: {}): Promise<string> // {}\n",
            called(&spec.name),
            type_of(&spec.parameters),
            first_sentence(&spec.description)
        );
        if out.len() + line.len() > budget {
            rest.push(spec.name.as_str());
        } else {
            out.push_str(&line);
        }
    }
    if !rest.is_empty() {
        out.push_str(&format!(
            "(also: {} — describeTool() gives their arguments)\n",
            rest.join(", ")
        ));
    }
    out
}

/// A JSON schema as a TypeScript-like type, one level deep.
fn type_of(schema: &Value) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return "{}".into();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    // Sorted: `serde_json` keeps a schema's order or not depending on a feature another crate may switch on, and a
    // host's tool description must not change between builds (a prompt's cached head).
    let mut properties: Vec<(&String, &Value)> = properties.iter().collect();
    properties.sort_by(|a, b| a.0.cmp(b.0));
    let fields: Vec<String> = properties
        .into_iter()
        .map(|(name, property)| {
            let optional = if required.contains(&name.as_str()) {
                ""
            } else {
                "?"
            };
            let kind = match property.get("type").and_then(Value::as_str) {
                Some("integer") => "number",
                Some("array") => "any[]",
                Some(other) => other,
                None => "any",
            };
            format!("{name}{optional}: {kind}")
        })
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// The text up to its first full stop followed by a space.
fn first_sentence(text: &str) -> &str {
    let line = text.split('\n').next().unwrap_or("");
    let end = line.find(". ").map_or(line.len(), |at| at + 1);
    line[..end].trim()
}

/// How a script calls `name`: `tools.name` when it is an identifier, `tools["name"]` when not.
fn called(name: &str) -> String {
    let mut chars = name.chars();
    let identifier = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if identifier {
        format!("tools.{name}")
    } else {
        format!("tools[{}]", Value::String(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    #[test]
    fn declarations_show_sorted_arguments_and_the_first_sentence_within_the_budget() {
        let specs = [
            spec(
                "read_file",
                "Read a file. Long ones in parts.",
                json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"}},"required":["path"]}),
            ),
            spec("find", "Find things.", json!({})),
        ];
        let all = declarations(&specs, 10_000);
        assert_eq!(
            all,
            "tools.read_file(args: {offset?: number, path: string}): Promise<string> // Read a file.\n\
             tools.find(args: {}): Promise<string> // Find things.\n"
        );
        let tight = declarations(&specs, 90);
        assert!(
            tight.ends_with("(also: find — describeTool() gives their arguments)\n"),
            "{tight}"
        );
    }

    #[test]
    fn search_needs_every_word_and_describe_gives_the_whole_spec() {
        let catalog = Catalog::new(vec![
            spec("read_calendar", "Reads the calendar. For days.", json!({})),
            spec("read_mail", "Reads mail.", json!({})),
        ]);
        assert_eq!(
            catalog.search("READ calendar"),
            json!([{"name": "read_calendar", "description": "Reads the calendar."}])
        );
        assert_eq!(catalog.describe("read_mail")["description"], "Reads mail.");
        assert_eq!(catalog.describe("nope"), Value::Null);
        assert_eq!(catalog.names(), json!(["read_calendar", "read_mail"]));
    }
}
