// Copyright 2018-2025 the Deno authors. MIT license.

use std::error::Error;
use std::fmt;

use deno_ast::ModuleSpecifier;
use deno_core::serde::Deserialize;
use deno_core::serde::Deserializer;
use deno_core::serde::Serialize;
use deno_core::serde::Serializer;
use deno_core::sourcemap::SourceMap;
use deno_graph::ModuleGraph;
use deno_terminal::colors;

const MAX_SOURCE_LINE_LENGTH: usize = 150;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticCategory {
  Warning,
  Error,
  Suggestion,
  Message,
}

impl fmt::Display for DiagnosticCategory {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(
      f,
      "{}",
      match self {
        DiagnosticCategory::Warning => "WARN ",
        DiagnosticCategory::Error => "ERROR ",
        DiagnosticCategory::Suggestion => "",
        DiagnosticCategory::Message => "",
      }
    )
  }
}

impl<'de> Deserialize<'de> for DiagnosticCategory {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    let s: i64 = Deserialize::deserialize(deserializer)?;
    Ok(DiagnosticCategory::from(s))
  }
}

impl Serialize for DiagnosticCategory {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let value = match self {
      DiagnosticCategory::Warning => 0_i32,
      DiagnosticCategory::Error => 1_i32,
      DiagnosticCategory::Suggestion => 2_i32,
      DiagnosticCategory::Message => 3_i32,
    };
    Serialize::serialize(&value, serializer)
  }
}

impl From<i64> for DiagnosticCategory {
  fn from(value: i64) -> Self {
    match value {
      0 => DiagnosticCategory::Warning,
      1 => DiagnosticCategory::Error,
      2 => DiagnosticCategory::Suggestion,
      3 => DiagnosticCategory::Message,
      _ => panic!("Unknown value: {value}"),
    }
  }
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticMessageChain {
  message_text: String,
  category: DiagnosticCategory,
  code: i64,
  next: Option<Vec<DiagnosticMessageChain>>,
}

impl DiagnosticMessageChain {
  pub fn format_message(&self, level: usize) -> String {
    let mut s = String::new();

    s.push_str(&" ".repeat(level * 2));
    s.push_str(&self.message_text);
    if let Some(next) = &self.next {
      let arr = next.clone();
      for dm in arr {
        s.push('\n');
        s.push_str(&dm.format_message(level + 1));
      }
    }

    s
  }
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Position {
  /// 0-indexed line number
  pub line: u64,
  /// 0-indexed character number
  pub character: u64,
}

impl Position {
  pub fn from_deno_graph(deno_graph_position: deno_graph::Position) -> Self {
    Self {
      line: deno_graph_position.line as u64,
      character: deno_graph_position.character as u64,
    }
  }
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
  pub category: DiagnosticCategory,
  pub code: u64,
  pub start: Option<Position>,
  pub end: Option<Position>,
  /// Position of this diagnostic in the original non-mapped source.
  ///
  /// This will exist and be different from the `start` for fast
  /// checked modules where the TypeScript source will differ
  /// from the original source.
  #[serde(skip_serializing)]
  pub original_source_start: Option<Position>,
  pub message_text: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub message_chain: Option<DiagnosticMessageChain>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub source: Option<String>,
  pub source_line: Option<String>,
  pub file_name: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub related_information: Option<Vec<Diagnostic>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reports_deprecated: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reports_unnecessary: Option<bool>,
  #[serde(flatten)]
  pub other: deno_core::serde_json::Map<String, deno_core::serde_json::Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub missing_specifier: Option<String>,
}

impl Diagnostic {
  pub fn from_missing_error(
    specifier: &str,
    maybe_range: Option<&deno_graph::Range>,
    additional_message: Option<String>,
  ) -> Self {
    Self {
      category: DiagnosticCategory::Error,
      code: 2307,
      start: maybe_range.map(|r| Position::from_deno_graph(r.range.start)),
      end: maybe_range.map(|r| Position::from_deno_graph(r.range.end)),
      original_source_start: None, // will be applied later
      message_text: Some(format!(
        "Cannot find module '{}'.{}{}",
        specifier,
        if additional_message.is_none() {
          ""
        } else {
          " "
        },
        additional_message.unwrap_or_default()
      )),
      message_chain: None,
      source: None,
      source_line: None,
      file_name: maybe_range.map(|r| r.specifier.to_string()),
      related_information: None,
      reports_deprecated: None,
      reports_unnecessary: None,
      other: Default::default(),
      missing_specifier: Some(specifier.to_string()),
    }
  }

  /// If this diagnostic should be included when it comes from a remote module.
  pub fn include_when_remote(&self) -> bool {
    /// TS6133: value is declared but its value is never read (noUnusedParameters and noUnusedLocals)
    const TS6133: u64 = 6133;
    /// TS4114: This member must have an 'override' modifier because it overrides a member in the base class 'X'.
    const TS4114: u64 = 4114;
    !matches!(self.code, TS6133 | TS4114)
  }

  fn fmt_category_and_code(&self, f: &mut fmt::Formatter) -> fmt::Result {
    let category = match self.category {
      DiagnosticCategory::Error => "ERROR",
      DiagnosticCategory::Warning => "WARN",
      _ => "",
    };

    let code = if self.code >= 900001 {
      "".to_string()
    } else {
      colors::bold(format!("TS{} ", self.code)).to_string()
    };

    if !category.is_empty() {
      write!(f, "{code}[{category}]: ")
    } else {
      Ok(())
    }
  }

  fn fmt_frame(&self, f: &mut fmt::Formatter, level: usize) -> fmt::Result {
    if let (Some(file_name), Some(start)) = (
      self.file_name.as_ref(),
      self.original_source_start.as_ref().or(self.start.as_ref()),
    ) {
      write!(
        f,
        "\n{:indent$}    at {}:{}:{}",
        "",
        colors::cyan(file_name),
        colors::yellow(&(start.line + 1).to_string()),
        colors::yellow(&(start.character + 1).to_string()),
        indent = level
      )
    } else {
      Ok(())
    }
  }

  fn fmt_message(&self, f: &mut fmt::Formatter, level: usize) -> fmt::Result {
    if let Some(message_chain) = &self.message_chain {
      write!(f, "{}", message_chain.format_message(level))
    } else {
      write!(
        f,
        "{:indent$}{}",
        "",
        self.message_text.as_deref().unwrap_or_default(),
        indent = level,
      )
    }
  }

  fn fmt_source_line(
    &self,
    f: &mut fmt::Formatter,
    level: usize,
  ) -> fmt::Result {
    if let (Some(source_line), Some(start), Some(end)) =
      (&self.source_line, &self.start, &self.end)
    {
      if !source_line.is_empty() && source_line.len() <= MAX_SOURCE_LINE_LENGTH
      {
        write!(f, "\n{:indent$}{}", "", source_line, indent = level)?;
        let length = if start.line == end.line {
          end.character - start.character
        } else {
          1
        };
        let mut s = String::new();
        for i in 0..start.character {
          s.push(if source_line.chars().nth(i as usize).unwrap() == '\t' {
            '\t'
          } else {
            ' '
          });
        }
        // TypeScript always uses `~` when underlining, but v8 always uses `^`.
        // We will use `^` to indicate a single point, or `~` when spanning
        // multiple characters.
        let ch = if length > 1 { '~' } else { '^' };
        for _i in 0..length {
          s.push(ch)
        }
        let underline = if self.is_error() {
          colors::red(&s).to_string()
        } else {
          colors::cyan(&s).to_string()
        };
        write!(f, "\n{:indent$}{}", "", underline, indent = level)?;
      }
    }

    Ok(())
  }

  fn fmt_related_information(&self, f: &mut fmt::Formatter) -> fmt::Result {
    if let Some(related_information) = self.related_information.as_ref() {
      if !related_information.is_empty() {
        write!(f, "\n\n")?;
        for info in related_information {
          info.fmt_stack(f, 4)?;
        }
      }
    }

    Ok(())
  }

  fn fmt_stack(&self, f: &mut fmt::Formatter, level: usize) -> fmt::Result {
    self.fmt_category_and_code(f)?;
    self.fmt_message(f, level)?;
    self.fmt_source_line(f, level)?;
    self.fmt_frame(f, level)
  }

  fn is_error(&self) -> bool {
    self.category == DiagnosticCategory::Error
  }
}

impl fmt::Display for Diagnostic {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    self.fmt_stack(f, 0)?;
    self.fmt_related_information(f)
  }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, deno_error::JsError)]
#[class(generic)]
pub struct Diagnostics(Vec<Diagnostic>);

impl Diagnostics {
  #[cfg(test)]
  pub fn new(diagnostics: Vec<Diagnostic>) -> Self {
    Diagnostics(diagnostics)
  }

  pub fn emit_warnings(&mut self) {
    self.0.retain(|d| {
      if d.category == DiagnosticCategory::Warning {
        log::warn!("{}\n", d);
        false
      } else {
        true
      }
    });
  }

  pub fn push(&mut self, diagnostic: Diagnostic) {
    self.0.push(diagnostic);
  }

  pub fn extend(&mut self, diagnostic: Diagnostics) {
    self.0.extend(diagnostic.0);
  }

  /// Return a set of diagnostics where only the values where the predicate
  /// returns `true` are included.
  pub fn filter(self, predicate: impl FnMut(&Diagnostic) -> bool) -> Self {
    let diagnostics = self.0.into_iter().filter(predicate).collect();
    Self(diagnostics)
  }

  pub fn retain(&mut self, predicate: impl FnMut(&Diagnostic) -> bool) {
    self.0.retain(predicate);
  }

  pub fn has_diagnostic(&self) -> bool {
    !self.0.is_empty()
  }

  /// Modifies all the diagnostics to have their display positions
  /// modified to point at the original source.
  pub fn apply_fast_check_source_maps(&mut self, graph: &ModuleGraph) {
    fn visit_diagnostic(d: &mut Diagnostic, graph: &ModuleGraph) {
      if let Some(specifier) = d
        .file_name
        .as_ref()
        .and_then(|n| ModuleSpecifier::parse(n).ok())
      {
        if let Ok(Some(module)) = graph.try_get_prefer_types(&specifier) {
          if let Some(fast_check_module) =
            module.js().and_then(|m| m.fast_check_module())
          {
            // todo(dsherret): use a short lived cache to prevent parsing
            // source maps so often
            if let Ok(source_map) =
              SourceMap::from_slice(fast_check_module.source_map.as_bytes())
            {
              if let Some(start) = d.start.as_mut() {
                let maybe_token = source_map
                  .lookup_token(start.line as u32, start.character as u32);
                if let Some(token) = maybe_token {
                  d.original_source_start = Some(Position {
                    line: token.get_src_line() as u64,
                    character: token.get_src_col() as u64,
                  });
                }
              }
            }
          }
        }
      }

      if let Some(related) = &mut d.related_information {
        for d in related.iter_mut() {
          visit_diagnostic(d, graph);
        }
      }
    }

    for d in &mut self.0 {
      visit_diagnostic(d, graph);
    }
  }
}

impl<'de> Deserialize<'de> for Diagnostics {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    let items: Vec<Diagnostic> = Deserialize::deserialize(deserializer)?;
    Ok(Diagnostics(items))
  }
}

impl Serialize for Diagnostics {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    Serialize::serialize(&self.0, serializer)
  }
}

impl fmt::Display for Diagnostics {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    display_diagnostics(f, self)?;
    if self.0.len() > 1 {
      write!(f, "\n\nFound {} errors.", self.0.len())?;
    }
    Ok(())
  }
}

fn display_diagnostics(
  f: &mut fmt::Formatter,
  diagnostics: &Diagnostics,
) -> fmt::Result {
  for (i, item) in diagnostics.0.iter().enumerate() {
    if i > 0 {
      write!(f, "\n\n")?;
    }
    write!(f, "{item}")?;
  }
  Ok(())
}

impl Error for Diagnostics {}

#[cfg(test)]
mod tests {
  use deno_core::serde_json;
  use deno_core::serde_json::json;
  use test_util::strip_ansi_codes;

  use super::*;

  #[test]
  fn test_de_diagnostics() {
    let value = json!([
      {
        "messageText": "Unknown compiler option 'invalid'.",
        "category": 1,
        "code": 5023
      },
      {
        "start": {
          "line": 0,
          "character": 0
        },
        "end": {
          "line": 0,
          "character": 7
        },
        "fileName": "test.ts",
        "messageText": "Cannot find name 'console'. Do you need to change your target library? Try changing the `lib` compiler option to include 'dom'.",
        "sourceLine": "console.log(\"a\");",
        "category": 1,
        "code": 2584
      },
      {
        "start": {
          "line": 7,
          "character": 0
        },
        "end": {
          "line": 7,
          "character": 7
        },
        "fileName": "test.ts",
        "messageText": "Cannot find name 'foo_Bar'. Did you mean 'foo_bar'?",
        "sourceLine": "foo_Bar();",
        "relatedInformation": [
          {
            "start": {
              "line": 3,
              "character": 9
            },
            "end": {
              "line": 3,
              "character": 16
            },
            "fileName": "test.ts",
            "messageText": "'foo_bar' is declared here.",
            "sourceLine": "function foo_bar() {",
            "category": 3,
            "code": 2728
          }
        ],
        "category": 1,
        "code": 2552
      },
      {
        "start": {
          "line": 18,
          "character": 0
        },
        "end": {
          "line": 18,
          "character": 1
        },
        "fileName": "test.ts",
        "messageChain": {
          "messageText": "Type '{ a: { b: { c(): { d: number; }; }; }; }' is not assignable to type '{ a: { b: { c(): { d: string; }; }; }; }'.",
          "category": 1,
          "code": 2322,
          "next": [
            {
              "messageText": "The types of 'a.b.c().d' are incompatible between these types.",
              "category": 1,
              "code": 2200,
              "next": [
                {
                  "messageText": "Type 'number' is not assignable to type 'string'.",
                  "category": 1,
                  "code": 2322
                }
              ]
            }
          ]
        },
        "sourceLine": "x = y;",
        "code": 2322,
        "category": 1
      }
    ]);
    let diagnostics: Diagnostics =
      serde_json::from_value(value).expect("cannot deserialize");
    assert_eq!(diagnostics.0.len(), 4);
    assert!(diagnostics.0[0].source_line.is_none());
    assert!(diagnostics.0[0].file_name.is_none());
    assert!(diagnostics.0[0].start.is_none());
    assert!(diagnostics.0[0].end.is_none());
    assert!(diagnostics.0[0].message_text.is_some());
    assert!(diagnostics.0[0].message_chain.is_none());
    assert!(diagnostics.0[0].related_information.is_none());
    assert!(diagnostics.0[1].source_line.is_some());
    assert!(diagnostics.0[1].file_name.is_some());
    assert!(diagnostics.0[1].start.is_some());
    assert!(diagnostics.0[1].end.is_some());
    assert!(diagnostics.0[1].message_text.is_some());
    assert!(diagnostics.0[1].message_chain.is_none());
    assert!(diagnostics.0[1].related_information.is_none());
    assert!(diagnostics.0[2].source_line.is_some());
    assert!(diagnostics.0[2].file_name.is_some());
    assert!(diagnostics.0[2].start.is_some());
    assert!(diagnostics.0[2].end.is_some());
    assert!(diagnostics.0[2].message_text.is_some());
    assert!(diagnostics.0[2].message_chain.is_none());
    assert!(diagnostics.0[2].related_information.is_some());
  }

  #[test]
  fn test_diagnostics_no_source() {
    let value = json!([
      {
        "messageText": "Unknown compiler option 'invalid'.",
        "category":1,
        "code":5023
      }
    ]);
    let diagnostics: Diagnostics = serde_json::from_value(value).unwrap();
    let actual = diagnostics.to_string();
    assert_eq!(
      strip_ansi_codes(&actual),
      "TS5023 [ERROR]: Unknown compiler option \'invalid\'."
    );
  }

  #[test]
  fn test_diagnostics_basic() {
    let value = json!([
      {
        "start": {
          "line": 0,
          "character": 0
        },
        "end": {
          "line": 0,
          "character": 7
        },
        "fileName": "test.ts",
        "messageText": "Cannot find name 'console'. Do you need to change your target library? Try changing the `lib` compiler option to include 'dom'.",
        "sourceLine": "console.log(\"a\");",
        "category": 1,
        "code": 2584
      }
    ]);
    let diagnostics: Diagnostics = serde_json::from_value(value).unwrap();
    let actual = diagnostics.to_string();
    assert_eq!(
      strip_ansi_codes(&actual),
      "TS2584 [ERROR]: Cannot find name \'console\'. Do you need to change your target library? Try changing the `lib` compiler option to include \'dom\'.\nconsole.log(\"a\");\n~~~~~~~\n    at test.ts:1:1"
    );
  }

  #[test]
  fn test_diagnostics_related_info() {
    let value = json!([
      {
        "start": {
          "line": 7,
          "character": 0
        },
        "end": {
          "line": 7,
          "character": 7
        },
        "fileName": "test.ts",
        "messageText": "Cannot find name 'foo_Bar'. Did you mean 'foo_bar'?",
        "sourceLine": "foo_Bar();",
        "relatedInformation": [
          {
            "start": {
              "line": 3,
              "character": 9
            },
            "end": {
              "line": 3,
              "character": 16
            },
            "fileName": "test.ts",
            "messageText": "'foo_bar' is declared here.",
            "sourceLine": "function foo_bar() {",
            "category": 3,
            "code": 2728
          }
        ],
        "category": 1,
        "code": 2552
      }
    ]);
    let diagnostics: Diagnostics = serde_json::from_value(value).unwrap();
    let actual = diagnostics.to_string();
    assert_eq!(
      strip_ansi_codes(&actual),
      "TS2552 [ERROR]: Cannot find name \'foo_Bar\'. Did you mean \'foo_bar\'?\nfoo_Bar();\n~~~~~~~\n    at test.ts:8:1\n\n    \'foo_bar\' is declared here.\n    function foo_bar() {\n             ~~~~~~~\n        at test.ts:4:10"
    );
  }
}
