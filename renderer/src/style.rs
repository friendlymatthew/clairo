use std::{collections::HashMap, rc::Rc};

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser, Token,
};
use html5ever::Attribute;
use markup5ever_rcdom::{Handle, Node, NodeData};

#[derive(Clone, Copy)]
pub(crate) enum Display {
    Block,
    Grid,
    Inline,
    None,
}

#[derive(Clone, Copy)]
pub(crate) enum LineHeight {
    Normal,
    Px(f32),
    Number(f32),
}

#[derive(Clone, Copy)]
pub(crate) enum Width {
    Auto,
    Px(f32),
    Percent(f32),
    Em(f32),
    Rem(f32),
}

#[derive(Clone, Copy)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

const SIDES: [Side; 4] = [Side::Top, Side::Right, Side::Bottom, Side::Left];

pub(crate) type Color = [f32; 3];
pub(crate) const INK: Color = [0.015, 0.016, 0.017];
pub(crate) const LINK: Color = [0.033, 0.091, 0.604];

#[derive(Clone, Copy)]
pub(crate) enum TextAlign {
    Start,
    Center,
    End,
}

pub(crate) struct ComputedStyle {
    pub(crate) display: Display,
    pub(crate) font_size_px: f32,
    pub(crate) line_height: LineHeight,
    pub(crate) width: Width,
    pub(crate) max_width: Width,
    pub(crate) first_grid_track_max: Width,
    pub(crate) margin: [Width; 4],
    pub(crate) padding: [Width; 4],
    pub(crate) border_box: bool,
    pub(crate) color: Color,
    pub(crate) background: Option<Color>,
    pub(crate) border_bottom: Option<Color>,
    pub(crate) text_align: TextAlign,
    pub(crate) float_right: bool,
    pub(crate) superscript: bool,
}

pub(crate) type LinkedStylesheets = HashMap<String, String>;

#[derive(Default)]
pub(crate) struct ElementStyles(HashMap<*const Node, ComputedStyle>);

impl ElementStyles {
    pub(crate) fn compute(root: &Handle, rules: &StyleRules, viewport_width: f32) -> Self {
        let mut styles = Self::default();
        let base_style = ComputedStyle {
            display: Display::Inline,
            font_size_px: 16.0,
            line_height: LineHeight::Normal,
            width: Width::Auto,
            max_width: Width::Auto,
            first_grid_track_max: Width::Auto,
            margin: [Width::Px(0.0); 4],
            padding: [Width::Px(0.0); 4],
            border_box: false,
            color: INK,
            background: None,
            border_bottom: None,
            text_align: TextAlign::Start,
            float_right: false,
            superscript: false,
        };

        walk(
            root,
            &base_style,
            16.0,
            rules,
            viewport_width,
            &mut Vec::new(),
            &mut styles,
        );
        styles
    }

    pub(crate) fn get(&self, node: &Handle) -> Option<&ComputedStyle> {
        self.0.get(&Rc::as_ptr(node))
    }
}

fn default_display(tag: &str) -> Display {
    match tag {
        "head" | "script" | "style" | "title" | "template" => Display::None,
        "html" | "body" | "main" | "article" | "section" | "aside" | "header" | "footer"
        | "nav" | "div" | "blockquote" | "figure" | "figcaption" | "ul" | "ol" | "li" | "dl"
        | "dt" | "dd" | "table" | "tr" | "th" | "td" | "caption" | "p" | "h1" | "h2" | "h3"
        | "h4" | "h5" | "h6" => Display::Block,
        _ => Display::Inline,
    }
}

fn walk(
    node: &Handle,
    parent_style: &ComputedStyle,
    root_size: f32,
    rules: &StyleRules,
    viewport_width: f32,
    ancestors: &mut Vec<Handle>,
    styles: &mut ElementStyles,
) {
    if let NodeData::Element { name, attrs, .. } = &node.data {
        let tag: &str = name.local.as_ref();
        let mut style = ComputedStyle {
            display: default_display(tag),
            font_size_px: parent_style.font_size_px,
            line_height: parent_style.line_height,
            width: Width::Auto,
            max_width: Width::Auto,
            first_grid_track_max: Width::Auto,
            margin: [Width::Px(0.0); 4],
            padding: [Width::Px(0.0); 4],
            border_box: false,
            color: parent_style.color,
            background: None,
            border_bottom: None,
            text_align: parent_style.text_align,
            float_right: false,
            superscript: parent_style.superscript,
        };

        match tag {
            "h1" => style.font_size_px *= 2.0,
            "h2" => style.font_size_px *= 1.5,
            "h3" => style.font_size_px *= 1.17,
            "sup" => {
                style.font_size_px *= 0.75;
                style.superscript = true;
            }
            "a" => style.color = LINK,
            _ => {}
        }

        let attrs = attrs.borrow();
        let mut declarations = rules.for_element(tag, &attrs, ancestors, viewport_width);
        for attr in attrs.iter().filter(|attr| attr.name.ns.is_empty()) {
            match attr.name.local.as_ref() {
                "hidden" => style.display = Display::None,
                "style" => {
                    let mut input = Parser::new(&attr.value);
                    for (order, value) in
                        RuleBodyParser::new(&mut input, &mut StyleDeclarationParser)
                            .flatten()
                            .enumerate()
                    {
                        for declaration in value.declarations {
                            declarations.insert(CascadedDeclaration {
                                declaration,
                                priority: (value.important, (usize::MAX, 0, 0), order),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        declarations.apply(&mut style, parent_style.font_size_px, root_size);
        let root_size = if tag == "html" {
            style.font_size_px
        } else {
            root_size
        };

        drop(attrs);
        ancestors.push(node.clone());
        for child in node.children.borrow().iter() {
            walk(
                child,
                &style,
                root_size,
                rules,
                viewport_width,
                ancestors,
                styles,
            );
        }
        ancestors.pop();

        styles.0.insert(Rc::as_ptr(node), style);

        return;
    }

    for child in node.children.borrow().iter() {
        walk(
            child,
            parent_style,
            root_size,
            rules,
            viewport_width,
            ancestors,
            styles,
        );
    }
}

#[derive(Clone, Copy)]
enum Declaration {
    Width(Width),
    MaxWidth(Width),
    FirstGridTrackMax(Width),
    Margin(Side, Width),
    Padding(Side, Width),
    BorderBox(bool),
    Display(Display),
    FontSize(Width),
    Color(Color),
    Background(Option<Color>),
    BorderBottom(Option<Color>),
    TextAlign(TextAlign),
    FloatRight(bool),
    LineHeight(LineHeight),
}

#[derive(Clone)]
struct DeclaredValue {
    declarations: Vec<Declaration>,
    important: bool,
}

#[derive(Default)]
struct CompoundSelector {
    tag: Option<String>,
    classes: Vec<String>,
    ids: Vec<String>,
}

type Specificity = (usize, usize, usize);

impl CompoundSelector {
    fn specificity(&self) -> Specificity {
        (
            self.ids.len(),
            self.classes.len(),
            usize::from(self.tag.is_some()),
        )
    }

    fn matches(&self, tag: &str, id: Option<&str>, classes: &str) -> bool {
        self.tag.as_deref().is_none_or(|expected| expected == tag)
            && self
                .ids
                .iter()
                .all(|expected| id == Some(expected.as_str()))
            && self
                .classes
                .iter()
                .all(|expected| classes.split(is_html_space).any(|class| class == expected))
    }
}

struct Selector {
    subject: CompoundSelector,
    conditions: Vec<(CompoundSelector, bool)>,
}

impl Selector {
    fn specificity(&self) -> Specificity {
        self.conditions
            .iter()
            .fold(self.subject.specificity(), |(a, b, c), (part, _)| {
                let (ids, classes, tags) = part.specificity();
                (a + ids, b + classes, c + tags)
            })
    }

    fn matches_ancestors(&self, ancestors: &[Handle]) -> bool {
        fn matches(conditions: &[(CompoundSelector, bool)], ancestors: &[Handle]) -> bool {
            let Some(((selector, direct), previous)) = conditions.split_last() else {
                return true;
            };
            let limit = if *direct { 1 } else { ancestors.len() };
            ancestors
                .iter()
                .enumerate()
                .rev()
                .take(limit)
                .any(|(index, node)| {
                    let NodeData::Element { name, attrs, .. } = &node.data else {
                        return false;
                    };
                    let attrs = attrs.borrow();
                    let (id, classes) = element_keys(&attrs);
                    selector.matches(name.local.as_ref(), id, classes)
                        && matches(previous, &ancestors[..index])
                })
        }
        matches(&self.conditions, ancestors)
    }
}

fn element_keys(attrs: &[Attribute]) -> (Option<&str>, &str) {
    let mut id = None;
    let mut classes = "";
    for attr in attrs.iter().filter(|attr| attr.name.ns.is_empty()) {
        match attr.name.local.as_ref() {
            "class" => classes = attr.value.as_ref(),
            "id" => id = Some(attr.value.as_ref()),
            _ => {}
        }
    }
    (id, classes)
}

#[derive(Clone, Copy)]
struct CascadedDeclaration {
    declaration: Declaration,
    priority: (bool, Specificity, usize),
}

#[derive(Default)]
struct CascadedDeclarations {
    width: Option<CascadedDeclaration>,
    max_width: Option<CascadedDeclaration>,
    first_grid_track_max: Option<CascadedDeclaration>,
    margin: [Option<CascadedDeclaration>; 4],
    padding: [Option<CascadedDeclaration>; 4],
    border_box: Option<CascadedDeclaration>,
    display: Option<CascadedDeclaration>,
    font_size: Option<CascadedDeclaration>,
    color: Option<CascadedDeclaration>,
    background: Option<CascadedDeclaration>,
    border_bottom: Option<CascadedDeclaration>,
    text_align: Option<CascadedDeclaration>,
    float_right: Option<CascadedDeclaration>,
    line_height: Option<CascadedDeclaration>,
}

impl CascadedDeclarations {
    fn insert(&mut self, value: CascadedDeclaration) {
        let slot = match value.declaration {
            Declaration::Width(_) => &mut self.width,
            Declaration::MaxWidth(_) => &mut self.max_width,
            Declaration::FirstGridTrackMax(_) => &mut self.first_grid_track_max,
            Declaration::Margin(side, _) => &mut self.margin[side as usize],
            Declaration::Padding(side, _) => &mut self.padding[side as usize],
            Declaration::BorderBox(_) => &mut self.border_box,
            Declaration::Display(_) => &mut self.display,
            Declaration::FontSize(_) => &mut self.font_size,
            Declaration::Color(_) => &mut self.color,
            Declaration::Background(_) => &mut self.background,
            Declaration::BorderBottom(_) => &mut self.border_bottom,
            Declaration::TextAlign(_) => &mut self.text_align,
            Declaration::FloatRight(_) => &mut self.float_right,
            Declaration::LineHeight(_) => &mut self.line_height,
        };
        if slot.is_none_or(|current| value.priority >= current.priority) {
            *slot = Some(value);
        }
    }

    fn values(&self) -> impl Iterator<Item = CascadedDeclaration> {
        [
            self.width,
            self.max_width,
            self.first_grid_track_max,
            self.border_box,
            self.display,
            self.font_size,
            self.color,
            self.background,
            self.border_bottom,
            self.text_align,
            self.float_right,
            self.line_height,
        ]
        .into_iter()
        .chain(self.margin)
        .chain(self.padding)
        .flatten()
    }

    fn apply(&self, style: &mut ComputedStyle, parent_size: f32, root_size: f32) {
        for value in self.values() {
            match value.declaration {
                Declaration::Width(width) => style.width = width,
                Declaration::MaxWidth(width) => style.max_width = width,
                Declaration::FirstGridTrackMax(width) => style.first_grid_track_max = width,
                Declaration::Margin(side, width) => style.margin[side as usize] = width,
                Declaration::Padding(side, width) => style.padding[side as usize] = width,
                Declaration::BorderBox(border_box) => style.border_box = border_box,
                Declaration::Display(display) => style.display = display,
                Declaration::FontSize(size) => {
                    style.font_size_px = match size {
                        Width::Px(px) => px,
                        Width::Percent(percent) => parent_size * percent / 100.0,
                        Width::Em(em) => parent_size * em,
                        Width::Rem(rem) => root_size * rem,
                        Width::Auto => parent_size,
                    };
                }
                Declaration::Color(value) => style.color = value,
                Declaration::Background(value) => style.background = value,
                Declaration::BorderBottom(value) => style.border_bottom = value,
                Declaration::TextAlign(value) => style.text_align = value,
                Declaration::FloatRight(value) => style.float_right = value,
                Declaration::LineHeight(line_height) => style.line_height = line_height,
            }
        }
    }
}

struct StyleRule {
    min_viewport_width: f32,
    selector: Selector,
    declarations: CascadedDeclarations,
}

#[derive(Default)]
pub(crate) struct StyleRules {
    tags: HashMap<String, Vec<StyleRule>>,
    classes: HashMap<String, Vec<StyleRule>>,
    ids: HashMap<String, Vec<StyleRule>>,
    source_order: usize,
    breakpoints: Vec<f32>,
}

impl StyleRules {
    pub(crate) fn media_matches_changed(&self, old_width: f32, new_width: f32) -> bool {
        self.breakpoints
            .iter()
            .any(|&width| (old_width >= width) != (new_width >= width))
    }

    fn for_element(
        &self,
        tag: &str,
        attrs: &[Attribute],
        ancestors: &[Handle],
        viewport_width: f32,
    ) -> CascadedDeclarations {
        let (id, classes) = element_keys(attrs);

        let mut cascaded = CascadedDeclarations::default();
        let candidates = self
            .tags
            .get(tag)
            .into_iter()
            .chain(id.and_then(|id| self.ids.get(id)))
            .chain(
                classes
                    .split(is_html_space)
                    .filter_map(|class| self.classes.get(class)),
            )
            .flatten();

        for rule in candidates {
            if viewport_width >= rule.min_viewport_width
                && rule.selector.subject.matches(tag, id, classes)
                && rule.selector.matches_ancestors(ancestors)
            {
                for declaration in rule.declarations.values() {
                    cascaded.insert(declaration);
                }
            }
        }
        cascaded
    }

    fn add_stylesheet(&mut self, css: &str) {
        let mut input = Parser::new(css);
        for ParsedRule {
            selector,
            declarations,
            min_viewport_width,
        } in StyleSheetParser::new(&mut input, &mut StyleRuleParser)
            .flatten()
            .flatten()
        {
            if declarations.is_empty() {
                continue;
            }

            let specificity = selector.specificity();
            let mut cascaded = CascadedDeclarations::default();
            for value in declarations {
                for declaration in value.declarations {
                    cascaded.insert(CascadedDeclaration {
                        declaration,
                        priority: (value.important, specificity, self.source_order),
                    });
                }
                self.source_order += 1;
            }

            // Index by one required selector part, then check the whole selector on lookup.
            let (index, name) = if let Some(id) = selector.subject.ids.first() {
                (&mut self.ids, id)
            } else if let Some(class) = selector.subject.classes.first() {
                (&mut self.classes, class)
            } else if let Some(tag) = &selector.subject.tag {
                (&mut self.tags, tag)
            } else {
                continue;
            };

            if min_viewport_width > 0.0 && !self.breakpoints.contains(&min_viewport_width) {
                self.breakpoints.push(min_viewport_width);
            }
            index.entry(name.clone()).or_default().push(StyleRule {
                min_viewport_width,
                selector,
                declarations: cascaded,
            });
        }
    }
}

fn is_html_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0C')
}

fn is_print_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.name.local.as_ref() == "media" && attr.value.trim().eq_ignore_ascii_case("print")
    })
}

pub(crate) fn stylesheet_href(node: &Node) -> Option<String> {
    let NodeData::Element { name, attrs, .. } = &node.data else {
        return None;
    };
    if name.ns.as_ref() != "http://www.w3.org/1999/xhtml" || name.local.as_ref() != "link" {
        return None;
    }

    let attrs = attrs.borrow();
    if is_print_only(&attrs) {
        return None;
    }
    let rel = attrs
        .iter()
        .find(|attr| attr.name.local.as_ref() == "rel")?;
    let mut relations = rel.value.split_ascii_whitespace();
    if !relations
        .clone()
        .any(|rel| rel.eq_ignore_ascii_case("stylesheet"))
        || relations.any(|rel| rel.eq_ignore_ascii_case("alternate"))
        || attrs
            .iter()
            .any(|attr| attr.name.local.as_ref() == "disabled")
    {
        return None;
    }

    attrs
        .iter()
        .find(|attr| attr.name.local.as_ref() == "href")
        .map(|attr| attr.value.trim().to_owned())
        .filter(|href| !href.is_empty())
}

#[cfg(test)]
pub(crate) fn collect_rules(root: &Handle, linked: &LinkedStylesheets) -> StyleRules {
    collect_rules_with_defaults(root, linked, "")
}

pub(crate) fn collect_rules_with_defaults(
    root: &Handle,
    linked: &LinkedStylesheets,
    defaults: &str,
) -> StyleRules {
    let mut rules = StyleRules::default();
    rules.add_stylesheet(defaults);
    let mut pending = vec![root.clone()];

    while let Some(node) = pending.pop() {
        if matches!(&node.data, NodeData::Element { name, attrs, .. }
            if name.ns.as_ref() == "http://www.w3.org/1999/xhtml" && name.local.as_ref() == "style"
                && !is_print_only(&attrs.borrow()))
        {
            let mut css = String::new();
            for child in node.children.borrow().iter() {
                if let NodeData::Text { contents } = &child.data {
                    css.push_str(contents.borrow().as_ref());
                }
            }
            rules.add_stylesheet(&css);
            continue;
        }

        if let Some(css) = stylesheet_href(&node).and_then(|href| linked.get(&href)) {
            rules.add_stylesheet(css);
        }

        pending.extend(node.children.borrow().iter().rev().cloned());
    }

    rules
}

struct ParsedRule {
    selector: Selector,
    declarations: Vec<DeclaredValue>,
    min_viewport_width: f32,
}

struct StyleRuleParser;

impl<'i> AtRuleParser<'i> for StyleRuleParser {
    type Prelude = f32;
    type AtRule = Vec<ParsedRule>;
    type Error = ();

    fn parse_prelude(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<f32, ParseError<()>> {
        if !name.eq_ignore_ascii_case("media") {
            return Err(ParseError::unexpected_token());
        }
        if input
            .try_parse(|input| input.expect_ident_matching("screen"))
            .is_ok()
        {
            input.expect_ident_matching("and")?;
        }
        input.expect_parenthesis_block()?;
        input.parse_nested_block(|input| {
            input.expect_ident_matching("min-width")?;
            input.expect_colon()?;
            parse_px(input.next()?)
        })
    }

    fn parse_block(
        &mut self,
        min_width: f32,
        _: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<Self::AtRule, ParseError<()>> {
        Ok(StyleSheetParser::new(input, &mut StyleRuleParser)
            .flatten()
            .flatten()
            .map(|mut rule| {
                rule.min_viewport_width = rule.min_viewport_width.max(min_width);
                rule
            })
            .collect())
    }
}

impl<'i> QualifiedRuleParser<'i> for StyleRuleParser {
    type Prelude = Vec<Selector>;
    type QualifiedRule = Vec<ParsedRule>;
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Self::Prelude, ParseError<()>> {
        input.parse_comma_separated(parse_selector)
    }

    fn parse_block(
        &mut self,
        selectors: Self::Prelude,
        _: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<Self::QualifiedRule, ParseError<()>> {
        let declarations: Vec<_> = RuleBodyParser::new(input, &mut StyleDeclarationParser)
            .flatten()
            .collect();
        Ok(selectors
            .into_iter()
            .map(|selector| ParsedRule {
                selector,
                declarations: declarations.clone(),
                min_viewport_width: 0.0,
            })
            .collect())
    }
}

fn parse_selector(input: &mut Parser<'_>) -> Result<Selector, ParseError<()>> {
    let mut subject = parse_compound(input)?;
    let mut conditions = Vec::new();

    while !input.is_exhausted() {
        let direct = input.try_parse(|input| input.expect_delim('>')).is_ok();
        if !direct {
            match input.next_including_whitespace()? {
                Token::WhiteSpace(_) => {}
                _ => return Err(ParseError::unexpected_token()),
            }
        }
        conditions.push((subject, direct));
        subject = parse_compound(input)?;
    }
    Ok(Selector {
        subject,
        conditions,
    })
}

fn parse_compound(input: &mut Parser<'_>) -> Result<CompoundSelector, ParseError<()>> {
    input.skip_whitespace();
    let mut selector = CompoundSelector {
        tag: input
            .try_parse(|input| input.expect_ident().map(|tag| tag.to_ascii_lowercase()))
            .ok(),
        ..CompoundSelector::default()
    };

    while let Ok(()) = input.try_parse(|input| -> Result<(), ParseError<()>> {
        match input.next_including_whitespace()? {
            Token::IDHash(id) => selector.ids.push(id.to_string()),
            Token::Delim('.') => match input.next_including_whitespace()? {
                Token::Ident(class) => selector.classes.push(class.to_string()),
                _ => return Err(ParseError::unexpected_token()),
            },
            _ => return Err(ParseError::unexpected_token()),
        }
        Ok(())
    }) {}

    if selector.specificity() == (0, 0, 0) {
        return Err(ParseError::unexpected_token());
    }
    Ok(selector)
}

struct StyleDeclarationParser;

impl<'i> AtRuleParser<'i> for StyleDeclarationParser {
    type Prelude = ();
    type AtRule = DeclaredValue;
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for StyleDeclarationParser {
    type Prelude = ();
    type QualifiedRule = DeclaredValue;
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, DeclaredValue, ()> for StyleDeclarationParser {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

impl<'i> DeclarationParser<'i> for StyleDeclarationParser {
    type Declaration = DeclaredValue;
    type Error = ();

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _: &ParserState,
    ) -> Result<DeclaredValue, ParseError<()>> {
        let declarations = cssparser::match_ignore_ascii_case! { &name,
            "width" => vec![Declaration::Width(parse_width(input.next()?)?)],
            "max-width" => vec![Declaration::MaxWidth(match input.next()? {
                Token::Ident(value) if value.eq_ignore_ascii_case("none") => Width::Auto,
                token => parse_length(token, false)?,
            })],
            "grid-template" => vec![Declaration::FirstGridTrackMax(parse_grid_template(input)?)],
            "grid-template-columns" => vec![Declaration::FirstGridTrackMax(parse_first_grid_track(input)?)],
            "margin" => parse_edges(input, true, Declaration::Margin)?,
            "padding" => parse_edges(input, false, Declaration::Padding)?,
            "margin-top" => vec![Declaration::Margin(Side::Top, parse_margin(input.next()?)?)],
            "margin-right" => vec![Declaration::Margin(Side::Right, parse_margin(input.next()?)?)],
            "margin-bottom" => vec![Declaration::Margin(Side::Bottom, parse_margin(input.next()?)?)],
            "margin-left" => vec![Declaration::Margin(Side::Left, parse_margin(input.next()?)?)],
            "padding-top" => vec![Declaration::Padding(Side::Top, parse_length(input.next()?, false)?)],
            "padding-right" => vec![Declaration::Padding(Side::Right, parse_length(input.next()?, false)?)],
            "padding-bottom" => vec![Declaration::Padding(Side::Bottom, parse_length(input.next()?, false)?)],
            "padding-left" => vec![Declaration::Padding(Side::Left, parse_length(input.next()?, false)?)],
            "box-sizing" => vec![Declaration::BorderBox(match input.expect_ident()?.as_ref() {
                value if value.eq_ignore_ascii_case("border-box") => true,
                value if value.eq_ignore_ascii_case("content-box") => false,
                _ => return Err(ParseError::unexpected_token()),
            })],
            "display" => vec![Declaration::Display(match input.expect_ident()?.as_ref() {
                value if value.eq_ignore_ascii_case("none") => Display::None,
                value if value.eq_ignore_ascii_case("block") => Display::Block,
                value if value.eq_ignore_ascii_case("grid") => Display::Grid,
                value if value.eq_ignore_ascii_case("inline") => Display::Inline,
                _ => return Err(ParseError::unexpected_token()),
            })],
            "font-size" => vec![Declaration::FontSize(match input.next()? {
                Token::Ident(value) if value.eq_ignore_ascii_case("inherit") => Width::Auto,
                token => parse_length(token, false)?,
            })],
            "color" => vec![Declaration::Color(parse_color(input)?.ok_or_else(ParseError::unexpected_token)?)],
            "background-color" => vec![Declaration::Background(parse_color(input)?)],
            "border-bottom" => vec![Declaration::BorderBottom(parse_rule_color(input)?)],
            "text-align" => vec![Declaration::TextAlign(match input.expect_ident()?.as_ref() {
                "left" | "start" => TextAlign::Start,
                "center" => TextAlign::Center,
                "right" | "end" => TextAlign::End,
                _ => return Err(ParseError::unexpected_token()),
            })],
            "float" => vec![Declaration::FloatRight(match input.expect_ident()?.as_ref() {
                "right" => true,
                "none" => false,
                _ => return Err(ParseError::unexpected_token()),
            })],
            "line-height" => vec![Declaration::LineHeight(parse_line_height(input.next()?)?)],
            _ => return Err(ParseError::unexpected_token()),
        };

        let important = input.try_parse(cssparser::parse_important).is_ok();
        Ok(DeclaredValue {
            declarations,
            important,
        })
    }
}

// Recognize a bounded leading track for the normal-flow grid fallback.
fn parse_color(input: &mut Parser<'_>) -> Result<Option<Color>, ParseError<()>> {
    if input
        .try_parse(|input| input.expect_function_matching("var"))
        .is_ok()
    {
        return input.parse_nested_block(|input| {
            input.expect_ident()?;
            input.expect_comma()?;
            parse_color(input)
        });
    }
    match input.next()? {
        Token::Hash(hex) | Token::IDHash(hex) => {
            let (r, g, b, _) = cssparser::color::parse_hash_color(hex.as_bytes())
                .map_err(|_| ParseError::unexpected_token())?;
            let to_linear = |value: u8| {
                let value = f32::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            Ok(Some([r, g, b].map(to_linear)))
        }
        Token::Ident(value) if value.eq_ignore_ascii_case("transparent") => Ok(None),
        Token::Ident(value) if value.eq_ignore_ascii_case("black") => Ok(Some([0.0; 3])),
        Token::Ident(value) if value.eq_ignore_ascii_case("white") => Ok(Some([1.0; 3])),
        _ => Err(ParseError::unexpected_token()),
    }
}

fn parse_rule_color(input: &mut Parser<'_>) -> Result<Option<Color>, ParseError<()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(None);
    }
    let px = parse_px(input.next()?)?;
    input.expect_ident_matching("solid")?;
    let color = parse_color(input)?;
    Ok(if px > 0.0 { color } else { None })
}

fn parse_grid_template(input: &mut Parser<'_>) -> Result<Width, ParseError<()>> {
    while !matches!(input.next()?, Token::Delim('/')) {}
    parse_first_grid_track(input)
}

fn parse_first_grid_track(input: &mut Parser<'_>) -> Result<Width, ParseError<()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(Width::Auto);
    }

    input.expect_function_matching("minmax")?;
    let limit = input.parse_nested_block(|input| {
        if !matches!(input.next()?, Token::Number { value, .. } if *value == 0.0) {
            return Err(ParseError::unexpected_token());
        }
        input.expect_comma()?;
        let limit = match input.next()? {
            Token::Dimension { value, unit, .. }
                if *value == 1.0 && unit.eq_ignore_ascii_case("fr") =>
            {
                Width::Auto
            }
            token => parse_length(token, false)?,
        };
        Ok(limit)
    })?;
    while input
        .try_parse(|input| input.expect_ident_matching("min-content"))
        .is_ok()
    {}
    Ok(limit)
}

fn parse_edges(
    input: &mut Parser<'_>,
    margin: bool,
    declaration: fn(Side, Width) -> Declaration,
) -> Result<Vec<Declaration>, ParseError<()>> {
    let parse_edge = |input: &mut Parser<'_>| {
        let token = input.next()?;
        if margin {
            parse_margin(token)
        } else {
            parse_length(token, false)
        }
    };
    let top = parse_edge(input)?;
    let right = input.try_parse(parse_edge).ok();
    let bottom = input.try_parse(parse_edge).ok();
    let left = input.try_parse(parse_edge).ok();
    let right = right.unwrap_or(top);
    Ok(SIDES
        .into_iter()
        .zip([top, right, bottom.unwrap_or(top), left.unwrap_or(right)])
        .map(|(side, width)| declaration(side, width))
        .collect())
}

fn parse_margin(token: &Token<'_>) -> Result<Width, ParseError<()>> {
    match token {
        Token::Ident(value) if value.eq_ignore_ascii_case("auto") => Ok(Width::Auto),
        _ => parse_length(token, true),
    }
}

fn parse_px(token: &Token<'_>) -> Result<f32, ParseError<()>> {
    match token {
        Token::Dimension { value, unit, .. }
            if value.is_finite() && *value >= 0.0 && unit.eq_ignore_ascii_case("px") =>
        {
            Ok(*value)
        }
        Token::Number { value, .. } if *value == 0.0 => Ok(0.0),
        _ => Err(ParseError::unexpected_token()),
    }
}

fn parse_width(token: &Token<'_>) -> Result<Width, ParseError<()>> {
    match token {
        Token::Ident(value) if value.eq_ignore_ascii_case("auto") => Ok(Width::Auto),
        _ => parse_length(token, false),
    }
}

fn parse_length(token: &Token<'_>, allow_negative: bool) -> Result<Width, ParseError<()>> {
    let valid = |value: f32| value.is_finite() && (allow_negative || value >= 0.0);
    match token {
        Token::Dimension { value, unit, .. } if valid(*value) => {
            cssparser::match_ignore_ascii_case! { unit,
                "px" => Ok(Width::Px(*value)),
                "em" => Ok(Width::Em(*value)),
                "rem" => Ok(Width::Rem(*value)),
                _ => Err(ParseError::unexpected_token()),
            }
        }
        Token::Percentage { unit_value, .. } if valid(*unit_value * 100.0) => {
            Ok(Width::Percent(*unit_value * 100.0))
        }
        Token::Number { value, .. } if *value == 0.0 => Ok(Width::Px(0.0)),
        _ => Err(ParseError::unexpected_token()),
    }
}

fn parse_line_height(token: &Token<'_>) -> Result<LineHeight, ParseError<()>> {
    match token {
        Token::Ident(value) if value.eq_ignore_ascii_case("normal") => Ok(LineHeight::Normal),
        Token::Number { value, .. } if value.is_finite() && *value >= 0.0 => {
            Ok(LineHeight::Number(*value))
        }
        _ => parse_px(token).map(LineHeight::Px),
    }
}
