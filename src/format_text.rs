use anyhow::Result;
use anyhow::bail;
use oxc_allocator::Allocator;
use oxc_ast::ast::JSXAttributeValue;
use oxc_ast::ast::Program;
use oxc_ast::ast::StringLiteral;
use oxc_ast_visit::Visit;
use oxc_formatter::ArrowParentheses;
use oxc_formatter::AttributePosition;
use oxc_formatter::CustomGroupDefinition;
use oxc_formatter::EmbeddedLanguageFormatting;
use oxc_formatter::Expand;
use oxc_formatter::FormatOptions;
use oxc_formatter::Formatter;
use oxc_formatter::GroupEntry;
use oxc_formatter::IndentStyle;
use oxc_formatter::IndentWidth;
use oxc_formatter::LineEnding;
use oxc_formatter::LineWidth;
use oxc_formatter::OperatorPosition;
use oxc_formatter::QuoteProperties;
use oxc_formatter::QuoteStyle;
use oxc_formatter::Semicolons;
use oxc_formatter::SortImportsOptions;
use oxc_formatter::SortOrder;
use oxc_formatter::SortTailwindcssOptions;
use oxc_formatter::TrailingCommas;
use oxc_parser::ParseOptions;
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxc_span::Span;
use std::path::Path;

use crate::configuration::Configuration;
use crate::configuration::StringQuoteStyle;

pub fn format_text(file_path: &Path, input_text: &str, config: &Configuration) -> Result<Option<String>> {
  let source_type = match SourceType::from_path(file_path) {
    Ok(source_type) => source_type,
    Err(_) => return Ok(None),
  };

  let output = if config.experimental_bare_yield_snippets.unwrap_or(false) && is_bare_yield_snippet(input_text) {
    match format_bare_yield_snippet(input_text, source_type, config) {
      Some(result) => result?,
      None => format_program_or_bail(input_text, source_type, config)?,
    }
  } else {
    match format_program(input_text, source_type, config) {
      Ok(output) => output,
      Err(error_text) => {
        if config.experimental_bare_yield_snippets.unwrap_or(false) && input_text.contains("yield") {
          format_bare_yield_snippet(input_text, source_type, config).unwrap_or_else(|| bail!("{}", error_text))?
        } else {
          bail!("{}", error_text);
        }
      }
    }
  };

  if output == input_text {
    Ok(None)
  } else {
    Ok(Some(output))
  }
}

fn format_program_or_bail(input_text: &str, source_type: SourceType, config: &Configuration) -> Result<String> {
  format_program(input_text, source_type, config).map_err(|error_text| anyhow::anyhow!(error_text))
}

fn format_program(
  input_text: &str,
  source_type: SourceType,
  config: &Configuration,
) -> std::result::Result<String, String> {
  let allocator = Allocator::default();
  let parse_options = ParseOptions {
    preserve_parens: false,
    ..Default::default()
  };
  let parsed = Parser::new(&allocator, input_text, source_type)
    .with_options(parse_options)
    .parse();

  if !parsed.errors.is_empty() {
    return Err(format_parse_errors(&parsed.errors));
  }

  let options = build_format_options(config);
  let formatter = Formatter::new(&allocator, options);
  let output = formatter.build(&parsed.program);
  if config.quote_style == Some(StringQuoteStyle::AlwaysDouble) {
    enforce_always_double_quote_style(&output, source_type)
  } else {
    Ok(output)
  }
}

fn format_parse_errors<T: std::fmt::Display>(errors: &[T]) -> String {
  let mut error_text = String::new();
  for (i, error) in errors.iter().enumerate() {
    if i > 0 {
      error_text.push('\n');
    }
    error_text.push_str(&error.to_string());
  }
  error_text
}

fn enforce_always_double_quote_style(input_text: &str, source_type: SourceType) -> std::result::Result<String, String> {
  // `alwaysDouble` is opt-in, so the extra parse keeps rewriting syntax-aware without affecting the default path.
  let allocator = Allocator::default();
  let parsed = Parser::new(&allocator, input_text, source_type).parse();
  if !parsed.errors.is_empty() {
    return Err(format_parse_errors(&parsed.errors));
  }

  let spans = collect_string_literal_spans(&parsed.program);
  let mut replacements = Vec::new();
  for span in spans {
    let start = span.start as usize;
    let end = span.end as usize;
    let raw = &input_text[start..end];
    if let Some(double_quoted) = quote_single_string_literal_as_double(raw) {
      replacements.push((start, end, double_quoted));
    }
  }

  if replacements.is_empty() {
    return Ok(input_text.to_string());
  }

  replacements.sort_by_key(|(start, _, _)| *start);
  let mut output = input_text.to_string();
  for (start, end, replacement) in replacements.into_iter().rev() {
    output.replace_range(start..end, &replacement);
  }
  Ok(output)
}

fn collect_string_literal_spans(program: &Program) -> Vec<Span> {
  let mut collector = StringLiteralSpanCollector { spans: Vec::new() };
  collector.visit_program(program);
  collector.spans
}

struct StringLiteralSpanCollector {
  spans: Vec<Span>,
}

impl<'a> Visit<'a> for StringLiteralSpanCollector {
  fn visit_string_literal(&mut self, literal: &StringLiteral<'a>) {
    self.spans.push(literal.span);
  }

  fn visit_jsx_attribute_value(&mut self, value: &JSXAttributeValue<'a>) {
    match value {
      JSXAttributeValue::StringLiteral(_) => {}
      JSXAttributeValue::ExpressionContainer(container) => self.visit_jsx_expression_container(container),
      JSXAttributeValue::Element(element) => self.visit_jsx_element(element),
      JSXAttributeValue::Fragment(fragment) => self.visit_jsx_fragment(fragment),
    }
  }
}

fn quote_single_string_literal_as_double(raw: &str) -> Option<String> {
  if !raw.starts_with('\'') || !raw.ends_with('\'') {
    return None;
  }

  let content = &raw[1..raw.len() - 1];
  let mut output = String::with_capacity(raw.len() + 2);
  output.push('"');

  let mut copy_start = 0;
  let mut bytes = content.bytes().enumerate().peekable();
  while let Some((byte_index, byte)) = bytes.next() {
    match byte {
      b'\\' => {
        if let Some(&(escaped_index, escaped)) = bytes.peek() {
          bytes.next();
          if escaped == b'\'' {
            output.push_str(&content[copy_start..byte_index]);
            copy_start = escaped_index;
          }
        }
      }
      b'"' => {
        output.push_str(&content[copy_start..byte_index]);
        output.push('\\');
        copy_start = byte_index;
      }
      _ => {}
    }
  }

  output.push_str(&content[copy_start..]);
  output.push('"');
  Some(output)
}

fn is_bare_yield_snippet(input_text: &str) -> bool {
  input_text.trim_start().starts_with("yield")
}

fn format_bare_yield_snippet(
  input_text: &str,
  source_type: SourceType,
  config: &Configuration,
) -> Option<Result<String>> {
  let wrapped = format!(
    "function* __dprint_bare_yield_snippet__() {{\n{}\n}}\n",
    input_text.trim()
  );
  let formatted = format_program(&wrapped, source_type, config).ok()?;
  Some(
    extract_wrapped_generator_body(&formatted)
      .ok_or_else(|| anyhow::anyhow!("Failed to extract formatted bare yield snippet.")),
  )
}

fn extract_wrapped_generator_body(formatted: &str) -> Option<String> {
  let body_start = formatted.find("{\n")? + 2;
  let body_end = formatted.rfind("\n}")?;
  let body = &formatted[body_start..body_end];
  let indent = body
    .lines()
    .find_map(|line| {
      if line.trim().is_empty() {
        None
      } else {
        Some(&line[..line.len() - line.trim_start().len()])
      }
    })
    .unwrap_or("");

  let mut result = String::new();
  for line in body.lines() {
    if let Some(stripped) = line.strip_prefix(indent) {
      result.push_str(stripped);
    } else {
      result.push_str(line);
    }
    result.push('\n');
  }
  Some(result)
}

fn build_format_options(config: &Configuration) -> FormatOptions {
  let mut options = FormatOptions::default();

  if let Some(line_ending) = config.line_ending {
    options.line_ending = match line_ending {
      crate::configuration::LineEnding::Lf => LineEnding::Lf,
      crate::configuration::LineEnding::Cr => LineEnding::Cr,
      crate::configuration::LineEnding::Crlf => LineEnding::Crlf,
    };
  }

  if let Some(indent_style) = config.indent_style {
    options.indent_style = match indent_style {
      crate::configuration::IndentStyle::Tab => IndentStyle::Tab,
      crate::configuration::IndentStyle::Space => IndentStyle::Space,
    };
  }

  if let Some(value) = config.indent_width
    && let Ok(width) = IndentWidth::try_from(value)
  {
    options.indent_width = width;
  }

  if let Some(value) = config.line_width
    && let Ok(width) = LineWidth::try_from(value)
  {
    options.line_width = width;
  }

  if let Some(semicolons) = config.semicolons {
    options.semicolons = match semicolons {
      crate::configuration::Semicolons::Always => Semicolons::Always,
      crate::configuration::Semicolons::AsNeeded => Semicolons::AsNeeded,
    };
  }

  if let Some(quote_style) = config.quote_style {
    options.quote_style = match quote_style {
      StringQuoteStyle::Single => QuoteStyle::Single,
      StringQuoteStyle::Double | StringQuoteStyle::AlwaysDouble => QuoteStyle::Double,
    };
  }

  if let Some(quote_style) = config.jsx_quote_style {
    options.jsx_quote_style = match quote_style {
      crate::configuration::QuoteStyle::Single => QuoteStyle::Single,
      crate::configuration::QuoteStyle::Double => QuoteStyle::Double,
    };
  }

  if let Some(quote_properties) = config.quote_properties {
    options.quote_properties = match quote_properties {
      crate::configuration::QuoteProperties::AsNeeded => QuoteProperties::AsNeeded,
      crate::configuration::QuoteProperties::Preserve => QuoteProperties::Preserve,
      crate::configuration::QuoteProperties::Consistent => QuoteProperties::Consistent,
    };
  }

  if let Some(arrow_parens) = config.arrow_parentheses {
    options.arrow_parentheses = match arrow_parens {
      crate::configuration::ArrowParentheses::Always => ArrowParentheses::Always,
      crate::configuration::ArrowParentheses::AsNeeded => ArrowParentheses::AsNeeded,
    };
  }

  if let Some(trailing_commas) = config.trailing_commas {
    options.trailing_commas = match trailing_commas {
      crate::configuration::TrailingCommas::All => TrailingCommas::All,
      crate::configuration::TrailingCommas::Es5 => TrailingCommas::Es5,
      crate::configuration::TrailingCommas::None => TrailingCommas::None,
    };
  }

  if let Some(bracket_spacing) = config.bracket_spacing {
    options.bracket_spacing = bracket_spacing.into();
  }

  if let Some(bracket_same_line) = config.bracket_same_line {
    options.bracket_same_line = bracket_same_line.into();
  }

  if let Some(attribute_position) = config.attribute_position {
    options.attribute_position = match attribute_position {
      crate::configuration::AttributePosition::Auto => AttributePosition::Auto,
      crate::configuration::AttributePosition::Multiline => AttributePosition::Multiline,
    };
  }

  if let Some(expand) = config.expand {
    options.expand = match expand {
      crate::configuration::Expand::Auto => Expand::Auto,
      crate::configuration::Expand::Never => Expand::Never,
    };
  }

  if let Some(embedded_language_formatting) = config.embedded_language_formatting {
    options.embedded_language_formatting = match embedded_language_formatting {
      crate::configuration::EmbeddedLanguageFormatting::Auto => EmbeddedLanguageFormatting::Auto,
      crate::configuration::EmbeddedLanguageFormatting::Off => EmbeddedLanguageFormatting::Off,
    };
  }

  if let Some(operator_position) = config.experimental_operator_position {
    options.experimental_operator_position = match operator_position {
      crate::configuration::OperatorPosition::Start => OperatorPosition::Start,
      crate::configuration::OperatorPosition::End => OperatorPosition::End,
    };
  }

  if let Some(experimental_ternaries) = config.experimental_ternaries {
    options.experimental_ternaries = experimental_ternaries;
  }

  if let Some(ref sort_imports) = config.experimental_sort_imports {
    options.sort_imports = Some(SortImportsOptions {
      partition_by_newline: sort_imports.partition_by_newline,
      partition_by_comment: sort_imports.partition_by_comment,
      sort_side_effects: sort_imports.sort_side_effects,
      order: sort_imports
        .order
        .map(|o| match o {
          crate::configuration::SortOrder::Asc => SortOrder::Asc,
          crate::configuration::SortOrder::Desc => SortOrder::Desc,
        })
        .unwrap_or_default(),
      ignore_case: sort_imports.ignore_case.unwrap_or(true),
      newlines_between: sort_imports.newlines_between.unwrap_or(true),
      internal_pattern: sort_imports.internal_pattern.clone(),
      groups: sort_imports
        .groups
        .iter()
        .map(|group| group.iter().map(|s| GroupEntry::parse(s)).collect())
        .collect(),
      custom_groups: sort_imports
        .custom_groups
        .iter()
        .map(|g| CustomGroupDefinition {
          group_name: g.group_name.clone(),
          element_name_pattern: g.element_name_pattern.clone(),
          ..Default::default()
        })
        .collect(),
      newline_boundary_overrides: Vec::new(),
    });
  }

  if let Some(ref tailwindcss) = config.experimental_tailwindcss {
    options.sort_tailwindcss = Some(SortTailwindcssOptions {
      config: tailwindcss.config.clone(),
      stylesheet: tailwindcss.stylesheet.clone(),
      functions: tailwindcss.functions.clone(),
      attributes: tailwindcss.attributes.clone(),
      preserve_whitespace: tailwindcss.preserve_whitespace,
      preserve_duplicates: tailwindcss.preserve_duplicates,
    });
  }

  options
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn formats_basic_js() {
    let input = "const x=1";
    let config = crate::configuration::Configuration::default();
    let result = format_text(std::path::Path::new("test.js"), input, &config)
      .unwrap()
      .unwrap();
    assert_eq!(result, "const x = 1;\n");
  }

  #[test]
  fn formats_bare_yield_star_snippet_when_enabled() {
    let input = "yield* myThing()";
    let config = crate::configuration::Configuration {
      experimental_bare_yield_snippets: Some(true),
      ..Default::default()
    };
    let result = format_text(std::path::Path::new("test.ts"), input, &config)
      .unwrap()
      .unwrap();
    assert_eq!(result, "yield* myThing();\n");
  }

  #[test]
  fn bare_yield_star_snippet_is_interpreted_as_multiply_by_default() {
    let input = "yield* myThing()";
    let config = crate::configuration::Configuration::default();
    let result = format_text(std::path::Path::new("test.ts"), input, &config)
      .unwrap()
      .unwrap();
    assert_eq!(result, "yield * myThing();\n");
  }
}
