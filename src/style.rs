//! Cascade, inheritance and computed values.

use std::collections::HashMap;
use std::rc::Rc;

use crate::css::{self, Color, Declaration, MatchState, PseudoElement, Selector, Stylesheet, Tok};
use crate::dom::{Document, NodeData, NodeId};

pub const UA_CSS: &str = include_str!("ua.css");

// ---------------------------------------------------------------- value types

/// A length that may depend on the containing block: `px + pct% of base`, or a
/// `min()`/`max()`/`clamp()` of such lengths (interned, see `LenExpr`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Len {
    Auto,
    Val(f32, f32),
    Expr(u32),
}

/// Comparison functions whose arguments mix units, evaluated once the base is known.
#[derive(Debug, Clone, PartialEq)]
pub enum LenExpr {
    Min(Vec<Len>),
    Max(Vec<Len>),
    Clamp(Len, Len, Len),
}

fn len_exprs() -> std::sync::MutexGuard<'static, Vec<LenExpr>> {
    static E: std::sync::OnceLock<std::sync::Mutex<Vec<LenExpr>>> = std::sync::OnceLock::new();
    E.get_or_init(|| std::sync::Mutex::new(Vec::new())).lock().unwrap_or_else(|e| e.into_inner())
}

impl LenExpr {
    fn intern(self) -> Len {
        let mut all = len_exprs();
        if let Some(i) = all.iter().position(|e| *e == self) {
            return Len::Expr(i as u32);
        }
        all.push(self);
        Len::Expr(all.len() as u32 - 1)
    }

    fn get(i: u32) -> Option<LenExpr> {
        len_exprs().get(i as usize).cloned()
    }

    fn eval(&self, f: &dyn Fn(&Len) -> Option<f32>) -> Option<f32> {
        match self {
            LenExpr::Min(a) => a.iter().map(f).collect::<Option<Vec<f32>>>()?.into_iter().reduce(f32::min),
            LenExpr::Max(a) => a.iter().map(f).collect::<Option<Vec<f32>>>()?.into_iter().reduce(f32::max),
            LenExpr::Clamp(lo, v, hi) => {
                let (lo, v, hi) = (f(lo)?, f(v)?, f(hi)?);
                Some(v.min(hi).max(lo))
            }
        }
    }
}

impl Len {
    pub const ZERO: Len = Len::Val(0.0, 0.0);
    pub fn px(v: f32) -> Len {
        Len::Val(v, 0.0)
    }
    pub fn pct(v: f32) -> Len {
        Len::Val(0.0, v)
    }
    pub fn is_auto(&self) -> bool {
        matches!(self, Len::Auto)
    }
    /// Resolve against a base; `auto` resolves to `None`.
    pub fn resolve(&self, base: f32) -> Option<f32> {
        match *self {
            Len::Auto => None,
            Len::Val(px, pct) => Some(px + pct * base / 100.0),
            Len::Expr(i) => LenExpr::get(i)?.eval(&|l| l.resolve(base)),
        }
    }
    /// Resolve where an indefinite base makes percentages behave as auto.
    pub fn resolve_opt(&self, base: Option<f32>) -> Option<f32> {
        match *self {
            Len::Auto => None,
            Len::Val(px, pct) => {
                if pct == 0.0 {
                    Some(px)
                } else {
                    base.map(|b| px + pct * b / 100.0)
                }
            }
            Len::Expr(i) => LenExpr::get(i)?.eval(&|l| l.resolve_opt(base)),
        }
    }
    pub fn or_zero(&self, base: f32) -> f32 {
        self.resolve(base).unwrap_or(0.0)
    }
    pub fn to_css(&self) -> String {
        match *self {
            Len::Auto => "auto".into(),
            Len::Val(px, 0.0) => format!("{}px", css::fmt_num(px)),
            Len::Val(0.0, pct) => format!("{}%", css::fmt_num(pct)),
            Len::Val(px, pct) => format!("calc({}% + {}px)", css::fmt_num(pct), css::fmt_num(px)),
            Len::Expr(i) => match LenExpr::get(i) {
                Some(LenExpr::Min(a)) => format!("min({})", a.iter().map(|l| l.to_css()).collect::<Vec<_>>().join(", ")),
                Some(LenExpr::Max(a)) => format!("max({})", a.iter().map(|l| l.to_css()).collect::<Vec<_>>().join(", ")),
                Some(LenExpr::Clamp(a, b, c)) => format!("clamp({}, {}, {})", a.to_css(), b.to_css(), c.to_css()),
                None => "auto".into(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    ListItem,
    Table,
    InlineTable,
    TableRowGroup,
    TableRow,
    TableCell,
    TableCaption,
    TableColumn,
    Contents,
}

impl Display {
    pub fn is_inline_level(self) -> bool {
        matches!(self, Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::InlineTable)
    }
    pub fn name(self) -> &'static str {
        match self {
            Display::None => "none",
            Display::Block => "block",
            Display::Inline => "inline",
            Display::InlineBlock => "inline-block",
            Display::Flex => "flex",
            Display::InlineFlex => "inline-flex",
            Display::Grid => "grid",
            Display::InlineGrid => "inline-grid",
            Display::ListItem => "list-item",
            Display::Table => "table",
            Display::InlineTable => "inline-table",
            Display::TableRowGroup => "table-row-group",
            Display::TableRow => "table-row",
            Display::TableCell => "table-cell",
            Display::TableCaption => "table-caption",
            Display::TableColumn => "table-column",
            Display::Contents => "contents",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontFamily {
    Sans,
    Serif,
    Mono,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    Normal,
    Px(f32),
    Mult(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Right,
    Center,
    Justify,
    /// Legacy `<center>` / `align=center`: centres inline content and block-level children.
    WebkitCenter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    Pre,
    NoWrap,
    PreWrap,
    PreLine,
}

impl WhiteSpace {
    pub fn collapses(self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine)
    }
    pub fn wraps(self) -> bool {
        !matches!(self, WhiteSpace::Pre | WhiteSpace::NoWrap)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTransform {
    None,
    Upper,
    Lower,
    Capitalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Baseline,
    Middle,
    Top,
    Bottom,
    TextTop,
    TextBottom,
    Sub,
    Super,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Scroll,
    Auto,
}

impl Overflow {
    pub fn clips(self) -> bool {
        self != Overflow::Visible
    }
    pub fn scrolls(self) -> bool {
        matches!(self, Overflow::Scroll | Overflow::Auto)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    Default,
    Pointer,
    Text,
    Move,
    NotAllowed,
    Grab,
    Crosshair,
    ResizeEw,
    ResizeNs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStyle {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    DisclosureOpen,
    DisclosureClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

impl FlexDirection {
    pub fn is_row(self) -> bool {
        matches!(self, FlexDirection::Row | FlexDirection::RowReverse)
    }
    pub fn is_reverse(self) -> bool {
        matches!(self, FlexDirection::RowReverse | FlexDirection::ColumnReverse)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Auto,
    Stretch,
    Start,
    End,
    Center,
    Baseline,
}

/// Intrinsic sizing keywords for `width`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SizeKw {
    #[default]
    None,
    FitContent,
    MaxContent,
    MinContent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Track {
    Px(f32),
    Pct(f32),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    MinMax(Box<Track>, Box<Track>),
    /// A length that depends on the grid's size, like `min(100%, 14rem)` or
    /// `calc(50% - 8px)`; resolved during layout.
    Len(Len),
}

#[derive(Debug, Clone, PartialEq)]
pub enum GridTemplate {
    None,
    Tracks(Vec<Track>),
    /// repeat(auto-fill|auto-fit, minmax(min, max))
    AutoFill { min: Len, max: Track },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridLine {
    pub start: Option<i32>,
    pub span: u32,
}

impl Default for GridLine {
    fn default() -> Self {
        GridLine { start: None, span: 1 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Background {
    LinearGradient { angle: f32, stops: Vec<(Color, Option<f32>)> },
    RadialGradient { stops: Vec<(Color, Option<f32>)> },
    Url(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
    pub inset: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgSize {
    Auto,
    Cover,
    Contain,
}

/// `background-size` for one layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgSizeSpec {
    Auto,
    Cover,
    Contain,
    Explicit(Len, Len),
}

/// `background-position` for one layer, as offsets from the left/right and top/bottom edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BgPos {
    pub from_right: bool,
    pub x: Len,
    pub from_bottom: bool,
    pub y: Len,
}

impl Default for BgPos {
    fn default() -> Self {
        BgPos { from_right: false, x: Len::ZERO, from_bottom: false, y: Len::ZERO }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    pub display: Display,
    pub position: Position,
    pub float: Float,
    pub clear: bool,
    pub border_box: bool,
    pub width: Len,
    /// `width: fit-content | max-content | min-content` (with `width` itself `auto`).
    pub width_kw: SizeKw,
    pub height: Len,
    pub min_width: Len,
    pub min_height: Len,
    pub max_width: Len,
    pub max_height: Len,
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub border_width: [f32; 4],
    pub border_style: [BorderStyle; 4],
    pub border_color: [Option<Color>; 4],
    pub border_radius: [Len; 4],
    pub inset: [Len; 4],
    pub color: Color,
    pub background_color: Color,
    /// Background image layers, topmost first; the other lists cycle over them.
    pub background_images: Vec<Background>,
    pub background_sizes: Vec<BgSizeSpec>,
    pub background_positions: Vec<BgPos>,
    pub background_repeats: Vec<(bool, bool)>,
    pub appearance_none: bool,
    pub font_size: f32,
    pub font_weight: u16,
    pub italic: bool,
    pub font_family: FontFamily,
    pub font_family_name: String,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    pub underline: bool,
    pub line_through: bool,
    pub overline: bool,
    pub decoration_color: Option<Color>,
    pub text_transform: TextTransform,
    /// `word-break: break-all` or `line-break: anywhere`: a line may break between any characters.
    pub break_anywhere: bool,
    /// `overflow-wrap` / `word-wrap`: break words that don't fit (2 = `anywhere`, which also
    /// lowers the min-content width; 1 = `break-word`, which doesn't).
    pub overflow_wrap: u8,
    /// Which of `word-break` / `line-break` set `break_anywhere` (so either can unset it).
    pub break_sources: u8,
    pub white_space: WhiteSpace,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub text_indent: Len,
    pub vertical_align: VAlign,
    pub visible: bool,
    pub opacity: f32,
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub z_index: Option<i32>,
    /// `isolation: isolate`: a stacking context even with z-index auto.
    pub isolate: bool,
    pub cursor: Cursor,
    pub list_style: ListStyle,
    pub list_inside: bool,
    pub flex_direction: FlexDirection,
    pub flex_wrap: bool,
    pub justify_content: Justify,
    pub align_items: Align,
    pub align_self: Align,
    pub justify_items: Align,
    pub justify_self: Align,
    pub align_content: Align,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Len,
    pub row_gap: f32,
    pub column_gap: f32,
    pub order: i32,
    pub grid_columns: GridTemplate,
    pub grid_rows: GridTemplate,
    pub grid_column: GridLine,
    pub grid_row: GridLine,
    pub box_shadow: Vec<Shadow>,
    pub outline_width: f32,
    pub outline_color: Option<Color>,
    pub outline_style: BorderStyle,
    pub outline_offset: f32,
    pub content: Option<String>,
    pub pointer_events: bool,
    pub translate: (Len, Len),
    pub scale: f32,
    pub border_collapse: bool,
    pub border_spacing: f32,
    pub ellipsis: bool,
    pub aspect_ratio: Option<f32>,
    pub object_fit: BgSize,
    pub custom: Rc<HashMap<String, Vec<Tok>>>,
    /// SVG paint properties (inherited).
    pub svg: crate::svg::SvgCss,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        ComputedStyle {
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: false,
            border_box: false,
            width: Len::Auto,
            width_kw: SizeKw::None,
            height: Len::Auto,
            min_width: Len::Auto,
            min_height: Len::Auto,
            max_width: Len::Auto,
            max_height: Len::Auto,
            margin: [Len::ZERO; 4],
            padding: [Len::ZERO; 4],
            border_width: [0.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [None; 4],
            border_radius: [Len::ZERO; 4],
            inset: [Len::Auto; 4],
            color: Color::BLACK,
            background_color: Color::TRANSPARENT,
            background_images: vec![],
            background_sizes: vec![],
            background_positions: vec![],
            background_repeats: vec![],
            appearance_none: false,
            font_size: 16.0,
            font_weight: 400,
            italic: false,
            font_family: FontFamily::Serif,
            font_family_name: "serif".into(),
            line_height: LineHeight::Normal,
            text_align: TextAlign::Left,
            underline: false,
            line_through: false,
            overline: false,
            decoration_color: None,
            text_transform: TextTransform::None,
            break_anywhere: false,
            overflow_wrap: 0,
            break_sources: 0,
            white_space: WhiteSpace::Normal,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            text_indent: Len::ZERO,
            vertical_align: VAlign::Baseline,
            visible: true,
            opacity: 1.0,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            z_index: None,
            isolate: false,
            cursor: Cursor::Default,
            list_style: ListStyle::Disc,
            list_inside: false,
            flex_direction: FlexDirection::Row,
            flex_wrap: false,
            justify_content: Justify::Start,
            align_items: Align::Stretch,
            align_self: Align::Auto,
            justify_items: Align::Auto,
            justify_self: Align::Auto,
            align_content: Align::Stretch,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Len::Auto,
            row_gap: 0.0,
            column_gap: 0.0,
            order: 0,
            grid_columns: GridTemplate::None,
            grid_rows: GridTemplate::None,
            grid_column: GridLine::default(),
            grid_row: GridLine::default(),
            box_shadow: vec![],
            outline_width: 0.0,
            outline_color: None,
            outline_style: BorderStyle::None,
            outline_offset: 0.0,
            content: None,
            pointer_events: true,
            translate: (Len::ZERO, Len::ZERO),
            scale: 1.0,
            border_collapse: false,
            border_spacing: 0.0,
            ellipsis: false,
            aspect_ratio: None,
            object_fit: BgSize::Auto,
            custom: Rc::new(HashMap::new()),
            svg: Default::default(),
        }
    }
}

impl ComputedStyle {
    /// A fresh style for a child: inherited properties copied, others initial.
    pub fn inherit(parent: &ComputedStyle) -> ComputedStyle {
        ComputedStyle {
            color: parent.color,
            font_size: parent.font_size,
            font_weight: parent.font_weight,
            italic: parent.italic,
            font_family: parent.font_family,
            font_family_name: parent.font_family_name.clone(),
            line_height: parent.line_height,
            text_align: parent.text_align,
            underline: parent.underline,
            line_through: parent.line_through,
            overline: parent.overline,
            decoration_color: parent.decoration_color,
            text_transform: parent.text_transform,
            break_anywhere: parent.break_anywhere,
            overflow_wrap: parent.overflow_wrap,
            break_sources: parent.break_sources,
            white_space: parent.white_space,
            letter_spacing: parent.letter_spacing,
            word_spacing: parent.word_spacing,
            text_indent: parent.text_indent,
            visible: parent.visible,
            cursor: parent.cursor,
            list_style: parent.list_style,
            list_inside: parent.list_inside,
            pointer_events: parent.pointer_events,
            border_collapse: parent.border_collapse,
            border_spacing: parent.border_spacing,
            custom: parent.custom.clone(),
            svg: parent.svg.clone(),
            ..ComputedStyle::default()
        }
    }

    pub fn line_height_px(&self) -> f32 {
        match self.line_height {
            LineHeight::Normal => (self.font_size * 1.2).round(),
            LineHeight::Px(p) => p,
            LineHeight::Mult(m) => self.font_size * m,
        }
    }

    pub fn is_positioned(&self) -> bool {
        self.position != Position::Static
    }

    pub fn is_out_of_flow(&self) -> bool {
        matches!(self.position, Position::Absolute | Position::Fixed)
    }

    pub fn border_color_at(&self, i: usize) -> Color {
        self.border_color[i].unwrap_or(self.color)
    }

    pub fn has_border(&self) -> bool {
        (0..4).any(|i| self.border_width[i] > 0.0 && self.border_style[i] != BorderStyle::None)
    }

    pub fn effective_border(&self, i: usize) -> f32 {
        if self.border_style[i] == BorderStyle::None { 0.0 } else { self.border_width[i] }
    }

    /// Serialize the main properties, used by getComputedStyle and DevTools.
    pub fn to_properties(&self) -> Vec<(&'static str, String)> {
        let edges = |v: &[Len; 4]| v.iter().map(|l| l.to_css()).collect::<Vec<_>>().join(" ");
        let fam = match self.font_family {
            FontFamily::Sans => "sans-serif",
            FontFamily::Serif => "serif",
            FontFamily::Mono => "monospace",
        };
        vec![
            ("display", self.display.name().into()),
            ("position", format!("{:?}", self.position).to_lowercase()),
            ("width", self.width.to_css()),
            ("height", self.height.to_css()),
            ("margin", edges(&self.margin)),
            ("padding", edges(&self.padding)),
            ("border-width", self.border_width.iter().map(|w| format!("{}px", css::fmt_num(*w))).collect::<Vec<_>>().join(" ")),
            ("border-color", self.border_color_at(0).to_css()),
            ("border-radius", edges(&self.border_radius)),
            ("color", self.color.to_css()),
            ("background-color", self.background_color.to_css()),
            ("font-size", format!("{}px", css::fmt_num(self.font_size))),
            ("font-weight", self.font_weight.to_string()),
            ("font-style", if self.italic { "italic".into() } else { "normal".into() }),
            ("font-family", fam.into()),
            ("line-height", format!("{}px", css::fmt_num(self.line_height_px()))),
            ("text-align", format!("{:?}", self.text_align).to_lowercase()),
            ("white-space", format!("{:?}", self.white_space).to_lowercase()),
            ("opacity", css::fmt_num(self.opacity)),
            ("visibility", if self.visible { "visible".into() } else { "hidden".into() }),
            ("overflow", format!("{:?}", self.overflow_y).to_lowercase()),
            ("z-index", self.z_index.map(|z| z.to_string()).unwrap_or("auto".into())),
            ("flex-direction", format!("{:?}", self.flex_direction).to_lowercase()),
            ("justify-content", format!("{:?}", self.justify_content).to_lowercase()),
            ("align-items", format!("{:?}", self.align_items).to_lowercase()),
            ("flex-grow", css::fmt_num(self.flex_grow)),
            ("flex-shrink", css::fmt_num(self.flex_shrink)),
            ("gap", format!("{}px {}px", css::fmt_num(self.row_gap), css::fmt_num(self.column_gap))),
            ("cursor", format!("{:?}", self.cursor).to_lowercase()),
            ("top", self.inset[0].to_css()),
            ("right", self.inset[1].to_css()),
            ("bottom", self.inset[2].to_css()),
            ("left", self.inset[3].to_css()),
        ]
    }

    pub fn get_property(&self, name: &str) -> String {
        if name.starts_with("--") {
            return self.custom.get(name).map(|t| css::tokens_to_string(t)).unwrap_or_default();
        }
        let name = match name {
            "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
                let i = ["top", "right", "bottom", "left"].iter().position(|s| name.ends_with(s)).unwrap();
                return self.margin[i].to_css();
            }
            "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
                let i = ["top", "right", "bottom", "left"].iter().position(|s| name.ends_with(s)).unwrap();
                return self.padding[i].to_css();
            }
            "background" => "background-color",
            "font-style" | "font-weight" | "font-size" | "color" => name,
            other => other,
        };
        self.to_properties().into_iter().find(|(k, _)| *k == name).map(|(_, v)| v).unwrap_or_default()
    }
}

// ---------------------------------------------------------------- cascade

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    UserAgent,
    Author,
}

struct RuleEntry {
    text: String,
    selector: Selector,
    spec: (u32, u32, u32),
    order: usize,
    origin: Origin,
    decls: Rc<Vec<Declaration>>,
    media: Option<css::MediaQuery>,
    sheet: usize,
    rule: usize,
}

/// Pre-indexed rules from all style sheets.
pub struct RuleSet {
    entries: Vec<RuleEntry>,
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
}

impl RuleSet {
    pub fn new(sheets: &[(Origin, Rc<Stylesheet>)]) -> RuleSet {
        let mut rs = RuleSet { entries: vec![], by_id: HashMap::new(), by_class: HashMap::new(), by_tag: HashMap::new(), universal: vec![] };
        let mut order = 0;
        for (si, (origin, sheet)) in sheets.iter().enumerate() {
            for (ri, rule) in sheet.rules.iter().enumerate() {
                let decls = Rc::new(rule.decls.clone());
                for sel in &rule.selectors {
                    let idx = rs.entries.len();
                    let key = &sel.parts[0].0;
                    if let Some(id) = &key.id {
                        rs.by_id.entry(id.clone()).or_default().push(idx);
                    } else if let Some(c) = key.classes.first() {
                        rs.by_class.entry(c.clone()).or_default().push(idx);
                    } else if let Some(t) = &key.tag {
                        rs.by_tag.entry(t.clone()).or_default().push(idx);
                    } else {
                        rs.universal.push(idx);
                    }
                    rs.entries.push(RuleEntry {
                        text: rule.selector_text.clone(),
                        selector: sel.clone(),
                        spec: sel.specificity(),
                        order,
                        origin: *origin,
                        decls: decls.clone(),
                        media: rule.media.clone(),
                        sheet: si,
                        rule: ri,
                    });
                    order += 1;
                }
            }
        }
        rs
    }

    fn candidates(&self, doc: &Document, node: NodeId) -> Vec<usize> {
        let mut c: Vec<usize> = self.universal.clone();
        if let Some(id) = doc.attr(node, "id") {
            if let Some(v) = self.by_id.get(id) {
                c.extend(v);
            }
        }
        for cls in doc.classes(node) {
            if let Some(v) = self.by_class.get(cls) {
                c.extend(v);
            }
        }
        if let Some(t) = doc.tag(node) {
            if let Some(v) = self.by_tag.get(t) {
                c.extend(v);
            }
        }
        c.sort_unstable();
        c.dedup();
        c
    }
}

/// A matched rule, reported to DevTools.
#[derive(Debug, Clone)]
pub struct MatchedRule {
    pub selector: String,
    pub origin: Origin,
    pub sheet: usize,
    pub rule: usize,
    pub decls: Vec<(String, String, bool)>,
}

/// Rules matching `node`, most specific last (for DevTools).
pub fn matched_rules(ctx: &StyleContext, node: NodeId) -> Vec<MatchedRule> {
    let mut out: Vec<(MatchedRule, (u8, (u32, u32, u32), usize))> = Vec::new();
    for idx in ctx.rules.candidates(ctx.doc, node) {
        let e = &ctx.rules.entries[idx];
        if e.selector.pseudo_element.is_some() {
            continue;
        }
        if let Some(m) = &e.media {
            if !m.matches(ctx.viewport.0, ctx.viewport.1) {
                continue;
            }
        }
        if !css::matches_selector(ctx.doc, node, &e.selector, &ctx.state) {
            continue;
        }
        let decls = e.decls.iter().map(|d| (d.name.clone(), d.value_string(), d.important)).collect();
        let level = if e.origin == Origin::UserAgent { 0 } else { 1 };
        out.push((MatchedRule { selector: e.text.clone(), origin: e.origin, sheet: e.sheet, rule: e.rule, decls }, (level, e.spec, e.order)));
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out.into_iter().map(|x| x.0).collect()
}

pub struct StyleContext<'a> {
    pub doc: &'a Document,
    pub rules: &'a RuleSet,
    pub state: MatchState,
    pub viewport: (f32, f32),
}

#[derive(Default)]
pub struct Styles {
    pub map: HashMap<NodeId, Rc<ComputedStyle>>,
    pub before: HashMap<NodeId, Rc<ComputedStyle>>,
    pub after: HashMap<NodeId, Rc<ComputedStyle>>,
}

impl Styles {
    pub fn get(&self, n: NodeId) -> Option<&Rc<ComputedStyle>> {
        self.map.get(&n)
    }
}

pub fn compute_styles(ctx: &StyleContext) -> Styles {
    let mut styles = Styles::default();
    let root_style = Rc::new(ComputedStyle::default());
    let mut root_font = 16.0;
    // Walk the tree, computing parents before children.
    let mut stack: Vec<(NodeId, Rc<ComputedStyle>)> = vec![(0, root_style)];
    while let Some((node, parent_style)) = stack.pop() {
        let style = if ctx.doc.is_element(node) {
            let (s, before, after) = compute_element(ctx, node, &parent_style, &mut root_font);
            let s = Rc::new(s);
            if let Some(b) = before {
                styles.before.insert(node, Rc::new(b));
            }
            if let Some(a) = after {
                styles.after.insert(node, Rc::new(a));
            }
            styles.map.insert(node, s.clone());
            s
        } else {
            parent_style.clone()
        };
        if matches!(ctx.doc.node(node).data, NodeData::Element(_) | NodeData::Document) {
            for &c in ctx.doc.children(node).iter().rev() {
                if ctx.doc.is_element(c) {
                    stack.push((c, style.clone()));
                }
            }
        }
    }
    // CSS Overflow: when the root element's overflow is visible, the body's overflow applies
    // to the viewport instead, and the body itself does not clip
    if let (Some(html), Some(body)) = (ctx.doc.children(0).iter().copied().find(|&n| ctx.doc.tag_is(n, "html")), ctx.doc.body()) {
        let root_visible = styles.map.get(&html).is_some_and(|s| s.overflow_x == Overflow::Visible && s.overflow_y == Overflow::Visible);
        if root_visible {
            if let Some(bs) = styles.map.get_mut(&body) {
                if bs.overflow_x != Overflow::Visible || bs.overflow_y != Overflow::Visible {
                    let mut b = (**bs).clone();
                    b.overflow_x = Overflow::Visible;
                    b.overflow_y = Overflow::Visible;
                    *bs = Rc::new(b);
                }
            }
        }
    }
    styles
}

fn matching_decls(ctx: &StyleContext, node: NodeId, pseudo: Option<PseudoElement>) -> Vec<(Declaration, u8, (u32, u32, u32), usize)> {
    let mut matched: Vec<(Declaration, u8, (u32, u32, u32), usize)> = Vec::new();
    for idx in ctx.rules.candidates(ctx.doc, node) {
        let e = &ctx.rules.entries[idx];
        if e.selector.pseudo_element != pseudo {
            continue;
        }
        if let Some(m) = &e.media {
            if !m.matches(ctx.viewport.0, ctx.viewport.1) {
                continue;
            }
        }
        if !css::matches_selector(ctx.doc, node, &e.selector, &ctx.state) {
            continue;
        }
        for d in e.decls.iter() {
            let level = match (e.origin, d.important) {
                (Origin::UserAgent, false) => 0,
                (Origin::Author, false) => 1,
                (Origin::Author, true) => 3,
                (Origin::UserAgent, true) => 5,
            };
            matched.push((d.clone(), level, e.spec, e.order));
        }
    }
    if pseudo.is_none() {
        if let Some(inline) = ctx.doc.attr(node, "style") {
            for d in css::parse_declarations(inline) {
                let level = if d.important { 4 } else { 2 };
                matched.push((d, level, (0, 0, 0), usize::MAX));
            }
        }
        // Presentational hints.
        for (name, value) in presentational_hints(ctx.doc, node) {
            matched.push((Declaration { name: name.into(), value: css::tokenize(&value), important: false }, 1, (0, 0, 0), 0));
        }
    }
    matched.sort_by(|a, b| (a.1, a.2, a.3).cmp(&(b.1, b.2, b.3)));
    matched
}

fn presentational_hints(doc: &Document, node: NodeId) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let px = |v: &str| -> String {
        let v = v.trim();
        if v.ends_with('%') { v.to_string() } else { format!("{}px", v.trim_end_matches("px")) }
    };
    if let Some(w) = doc.attr(node, "width") {
        if !doc.tag_is(node, "canvas") || true {
            out.push(("width", px(w)));
        }
    }
    if let Some(h) = doc.attr(node, "height") {
        out.push(("height", px(h)));
    }
    if let Some(a) = doc.attr(node, "align") {
        if matches!(doc.tag(node), Some("td" | "th" | "div" | "p" | "h1" | "h2" | "h3" | "h4" | "tr" | "tbody" | "thead" | "table")) {
            let v = match a.to_ascii_lowercase().as_str() {
                "center" | "middle" => "-webkit-center".to_string(),
                o => o.to_string(),
            };
            if doc.tag_is(node, "table") {
                // <table align=center> centres the table itself
                if v == "-webkit-center" {
                    out.push(("margin-left", "auto".into()));
                    out.push(("margin-right", "auto".into()));
                }
            } else {
                out.push(("text-align", v));
            }
        }
    }
    // <table cellpadding> pads every cell
    if matches!(doc.tag(node), Some("td" | "th")) {
        let mut cur = doc.parent(node);
        while let Some(n) = cur {
            if doc.tag_is(n, "table") {
                if let Some(cp) = doc.attr(n, "cellpadding") {
                    out.push(("padding", px(cp)));
                }
                break;
            }
            cur = doc.parent(n);
        }
    }
    // valign on cells, or inherited by cells from their row / row group
    if matches!(doc.tag(node), Some("td" | "th" | "tr" | "tbody" | "thead" | "tfoot")) {
        let mut cur = Some(node);
        while let Some(n) = cur {
            if let Some(v) = doc.attr(n, "valign") {
                let v = v.trim().to_ascii_lowercase();
                if matches!(v.as_str(), "top" | "middle" | "bottom" | "baseline") {
                    out.push(("vertical-align", v));
                }
                break;
            }
            if !matches!(doc.tag(n), Some("td" | "th" | "tr" | "tbody" | "thead" | "tfoot")) {
                break;
            }
            cur = doc.parent(n);
        }
    }
    if let Some(c) = doc.attr(node, "bgcolor") {
        out.push(("background-color", c.to_string()));
    }
    if doc.tag_is(node, "font") {
        if let Some(c) = doc.attr(node, "color") {
            out.push(("color", c.to_string()));
        }
    }
    if doc.tag_is(node, "table") {
        if let Some(b) = doc.attr(node, "border") {
            out.push(("border", format!("{}px outset gray", b.parse::<f32>().unwrap_or(1.0))));
        }
        if let Some(c) = doc.attr(node, "cellspacing") {
            out.push(("border-spacing", px(c)));
        }
    }
    out
}

fn compute_element(
    ctx: &StyleContext,
    node: NodeId,
    parent: &ComputedStyle,
    root_font: &mut f32,
) -> (ComputedStyle, Option<ComputedStyle>, Option<ComputedStyle>) {
    let decls = matching_decls(ctx, node, None);
    let mut style = ComputedStyle::inherit(parent);
    apply_all(&mut style, parent, &decls, *root_font, ctx.viewport);
    if ctx.doc.tag_is(node, "html") {
        *root_font = style.font_size;
    }
    fixup(&mut style, parent, ctx.doc.parent(node) == Some(0));
    let pseudo = |pe: PseudoElement| -> Option<ComputedStyle> {
        let d = matching_decls(ctx, node, Some(pe));
        if d.is_empty() {
            return None;
        }
        let mut s = ComputedStyle::inherit(&style);
        apply_all(&mut s, &style, &d, *root_font, ctx.viewport);
        fixup(&mut s, &style, false);
        let content = s.content.clone()?;
        let content = resolve_content(&content, ctx.doc, node);
        s.content = Some(content);
        if s.display == Display::None { None } else { Some(s) }
    };
    let before = pseudo(PseudoElement::Before);
    let after = pseudo(PseudoElement::After);
    (style, before, after)
}

fn resolve_content(raw: &str, doc: &Document, node: NodeId) -> String {
    // raw is a serialized token list: strings are quoted.
    let toks = css::tokenize(raw);
    let mut out = String::new();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Str(s) => out.push_str(s),
            Tok::Function(f) if f == "attr" => {
                if let Some(Tok::Ident(a)) = toks.get(i + 1) {
                    out.push_str(doc.attr(node, a).unwrap_or(""));
                }
                while i < toks.len() && toks[i] != Tok::RParen {
                    i += 1;
                }
            }
            Tok::Ident(s) if s == "open-quote" => out.push('“'),
            Tok::Ident(s) if s == "close-quote" => out.push('”'),
            _ => {}
        }
        i += 1;
    }
    out
}

fn fixup(style: &mut ComputedStyle, parent: &ComputedStyle, is_root: bool) {
    // Blockification.
    if style.float != Float::None || style.is_out_of_flow() || is_root {
        style.display = match style.display {
            Display::Inline | Display::InlineBlock | Display::TableRow | Display::TableRowGroup | Display::TableCell | Display::TableCaption => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            d => d,
        };
    }
    let _ = parent;
    for i in 0..4 {
        if style.border_style[i] == BorderStyle::None {
            style.border_width[i] = 0.0;
        }
    }
    if style.overflow_x != Overflow::Visible && style.overflow_y == Overflow::Visible {
        style.overflow_y = Overflow::Auto;
    }
    if style.overflow_y != Overflow::Visible && style.overflow_x == Overflow::Visible {
        style.overflow_x = Overflow::Auto;
    }
}

fn apply_all(style: &mut ComputedStyle, parent: &ComputedStyle, decls: &[(Declaration, u8, (u32, u32, u32), usize)], root_font: f32, vp: (f32, f32)) {
    // 1. custom properties
    let customs: Vec<&Declaration> = decls.iter().map(|d| &d.0).filter(|d| d.name.starts_with("--")).collect();
    if !customs.is_empty() {
        let mut map = (*style.custom).clone();
        for d in customs {
            let kw: Vec<&Tok> = d.value.iter().filter(|t| **t != Tok::Ws).collect();
            match kw.as_slice() {
                // the initial value of a custom property is the guaranteed-invalid value, so
                // var() falls back (Tailwind resets `--tw-*` this way)
                [Tok::Ident(k)] if k.eq_ignore_ascii_case("initial") => {
                    map.remove(&d.name);
                }
                [Tok::Ident(k)] if k.eq_ignore_ascii_case("inherit") || k.eq_ignore_ascii_case("unset") => match parent.custom.get(&d.name) {
                    Some(v) => {
                        map.insert(d.name.clone(), v.clone());
                    }
                    None => {
                        map.remove(&d.name);
                    }
                },
                _ => {
                    map.insert(d.name.clone(), d.value.clone());
                }
            }
        }
        // resolve var() references inside custom properties (one level of nesting is enough in practice)
        let snapshot = map.clone();
        for v in map.values_mut() {
            if v.iter().any(|t| matches!(t, Tok::Function(f) if f == "var")) {
                *v = substitute_vars(v, &snapshot, 0);
            }
        }
        style.custom = Rc::new(map);
    }
    let resolve = |d: &Declaration, style: &ComputedStyle| -> Vec<Tok> {
        if d.value.iter().any(|t| matches!(t, Tok::Function(f) if f == "var")) {
            substitute_vars(&d.value, &style.custom, 0)
        } else {
            d.value.clone()
        }
    };
    // 2. font-size / font shorthand first so em units resolve.
    for (d, ..) in decls {
        if d.name == "font-size" || d.name == "font" {
            let v = resolve(d, style);
            let cx = Ctx { font_size: parent.font_size, root_font, vp, parent_font: parent.font_size };
            apply_decl(style, parent, &d.name, &v, &cx);
        }
    }
    let cx = Ctx { font_size: style.font_size, root_font, vp, parent_font: parent.font_size };
    for (d, ..) in decls {
        if d.name.starts_with("--") || d.name == "font-size" {
            continue;
        }
        let v = resolve(d, style);
        apply_decl(style, parent, &d.name, &v, &cx);
    }
}

fn substitute_vars(toks: &[Tok], vars: &HashMap<String, Vec<Tok>>, depth: u32) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if let Tok::Function(f) = &toks[i] {
            if f == "var" && depth < 8 {
                // collect args until matching paren
                let mut j = i + 1;
                let mut d = 1;
                let mut args = Vec::new();
                while j < toks.len() {
                    match &toks[j] {
                        Tok::Function(_) | Tok::LParen => d += 1,
                        Tok::RParen => {
                            d -= 1;
                            if d == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    args.push(toks[j].clone());
                    j += 1;
                }
                let name = args.iter().find_map(|t| if let Tok::Ident(n) = t { Some(n.clone()) } else { None }).unwrap_or_default();
                let fallback: Vec<Tok> = match args.iter().position(|t| *t == Tok::Comma) {
                    Some(p) => args[p + 1..].iter().skip_while(|t| **t == Tok::Ws).cloned().collect(),
                    None => vec![],
                };
                let val = vars.get(&name).cloned().unwrap_or(fallback);
                out.extend(substitute_vars(&val, vars, depth + 1));
                i = j + 1;
                continue;
            }
        }
        out.push(toks[i].clone());
        i += 1;
    }
    out
}

struct Ctx {
    font_size: f32,
    root_font: f32,
    vp: (f32, f32),
    parent_font: f32,
}

/// Split tokens into space/comma separated components, grouping function arguments.
fn components(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut out: Vec<Vec<Tok>> = Vec::new();
    let mut cur: Vec<Tok> = Vec::new();
    let mut depth = 0;
    for t in toks {
        match t {
            Tok::Ws if depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                continue;
            }
            Tok::Comma if depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(vec![Tok::Comma]);
                continue;
            }
            Tok::Delim('/') if depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(vec![Tok::Delim('/')]);
                continue;
            }
            Tok::Function(_) | Tok::LParen => depth += 1,
            Tok::RParen => depth -= 1,
            _ => {}
        }
        cur.push(t.clone());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn split_commas(comps: &[Vec<Tok>]) -> Vec<Vec<Vec<Tok>>> {
    let mut out = vec![vec![]];
    for c in comps {
        if c.len() == 1 && c[0] == Tok::Comma {
            out.push(vec![]);
        } else {
            out.last_mut().unwrap().push(c.clone());
        }
    }
    out
}

fn ident(c: &[Tok]) -> Option<String> {
    match c {
        [Tok::Ident(s)] => Some(s.to_ascii_lowercase()),
        _ => None,
    }
}

fn unit_px(n: f32, unit: &str, cx: &Ctx) -> Option<f32> {
    Some(match unit {
        "px" => n,
        "em" => n * cx.font_size,
        "rem" => n * cx.root_font,
        "ex" => n * cx.font_size * 0.5,
        "ch" => n * cx.font_size * 0.5,
        "pt" => n * 96.0 / 72.0,
        "pc" => n * 16.0,
        "in" => n * 96.0,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "q" => n * 96.0 / 101.6,
        "vw" | "svw" | "lvw" | "dvw" => n * cx.vp.0 / 100.0,
        "vh" | "svh" | "lvh" | "dvh" => n * cx.vp.1 / 100.0,
        "vmin" => n * cx.vp.0.min(cx.vp.1) / 100.0,
        "vmax" => n * cx.vp.0.max(cx.vp.1) / 100.0,
        _ => return None,
    })
}

fn length(c: &[Tok], cx: &Ctx) -> Option<Len> {
    match c {
        [Tok::Number(n)] if *n == 0.0 => Some(Len::ZERO),
        [Tok::Number(n)] => Some(Len::px(*n)), // quirks: unitless
        [Tok::Dimension(n, u)] => unit_px(*n, u, cx).map(Len::px),
        [Tok::Percentage(p)] => Some(Len::pct(*p)),
        [Tok::Ident(s)] if s.eq_ignore_ascii_case("auto") => Some(Len::Auto),
        [Tok::Function(f), rest @ ..] => {
            let inner = &rest[..rest.len().saturating_sub(1)];
            match f.as_str() {
                "calc" | "-webkit-calc" => calc(inner, cx),
                "min" | "max" | "clamp" => {
                    let args: Vec<Len> = split_args(inner)
                        .iter()
                        .map(|a| {
                            let a: Vec<Tok> = a.iter().filter(|t| **t != Tok::Ws).cloned().collect();
                            length(&a, cx).filter(|l| !l.is_auto()).or_else(|| calc(&a, cx))
                        })
                        .collect::<Option<Vec<Len>>>()?;
                    pick_minmax(f, &args)
                }
                "var" => None,
                _ => None,
            }
        }
        _ => None,
    }
}

fn split_args(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut out = vec![vec![]];
    let mut depth = 0;
    for t in toks {
        match t {
            Tok::Comma if depth == 0 => {
                out.push(vec![]);
                continue;
            }
            Tok::Function(_) | Tok::LParen => depth += 1,
            Tok::RParen => depth -= 1,
            _ => {}
        }
        out.last_mut().unwrap().push(t.clone());
    }
    out
}

fn pick_minmax(f: &str, args: &[Len]) -> Option<Len> {
    if args.is_empty() {
        return None;
    }
    // all plain pixels: compute now; otherwise evaluate at layout time
    let pure: Vec<f32> = args.iter().filter_map(|a| if let Len::Val(px, 0.0) = a { Some(*px) } else { None }).collect();
    let all_pure = pure.len() == args.len();
    match f {
        "clamp" if args.len() == 3 => {
            if all_pure {
                return Some(Len::px(pure[1].min(pure[2]).max(pure[0])));
            }
            Some(LenExpr::Clamp(args[0], args[1], args[2]).intern())
        }
        "min" if all_pure => pure.into_iter().reduce(f32::min).map(Len::px),
        "max" if all_pure => pure.into_iter().reduce(f32::max).map(Len::px),
        "min" => Some(LenExpr::Min(args.to_vec()).intern()),
        "max" => Some(LenExpr::Max(args.to_vec()).intern()),
        _ => None,
    }
}

/// calc() over linear (px, pct) values.
fn calc(toks: &[Tok], cx: &Ctx) -> Option<Len> {
    let toks: Vec<Tok> = toks.to_vec();
    let mut pos = 0;
    let v = calc_sum(&toks, &mut pos, cx)?;
    match v {
        CalcVal::Len(px, pct) => Some(Len::Val(px, pct)),
        CalcVal::Num(n) => Some(Len::px(n)),
    }
}

#[derive(Clone, Copy)]
enum CalcVal {
    Len(f32, f32),
    Num(f32),
}

fn calc_skip_ws(t: &[Tok], p: &mut usize) {
    while *p < t.len() && t[*p] == Tok::Ws {
        *p += 1;
    }
}

fn calc_sum(t: &[Tok], p: &mut usize, cx: &Ctx) -> Option<CalcVal> {
    let mut acc = calc_prod(t, p, cx)?;
    loop {
        calc_skip_ws(t, p);
        let op = match t.get(*p) {
            Some(Tok::Delim('+')) => 1.0,
            Some(Tok::Delim('-')) => -1.0,
            Some(Tok::Number(n)) if *n < 0.0 => {
                // "a -1" style without spaces is invalid in CSS but be lenient
                let rhs = CalcVal::Num(*n);
                *p += 1;
                acc = calc_add(acc, rhs, 1.0)?;
                continue;
            }
            Some(Tok::Dimension(n, _)) if *n < 0.0 => {
                let rhs = calc_atom(t, p, cx)?;
                acc = calc_add(acc, rhs, 1.0)?;
                continue;
            }
            _ => return Some(acc),
        };
        *p += 1;
        let rhs = calc_prod(t, p, cx)?;
        acc = calc_add(acc, rhs, op)?;
    }
}

fn calc_add(a: CalcVal, b: CalcVal, sign: f32) -> Option<CalcVal> {
    Some(match (a, b) {
        (CalcVal::Num(x), CalcVal::Num(y)) => CalcVal::Num(x + sign * y),
        (CalcVal::Len(px, pc), CalcVal::Len(px2, pc2)) => CalcVal::Len(px + sign * px2, pc + sign * pc2),
        (CalcVal::Len(px, pc), CalcVal::Num(n)) => CalcVal::Len(px + sign * n, pc),
        (CalcVal::Num(n), CalcVal::Len(px, pc)) => CalcVal::Len(n + sign * px, sign * pc),
    })
}

fn calc_prod(t: &[Tok], p: &mut usize, cx: &Ctx) -> Option<CalcVal> {
    let mut acc = calc_atom(t, p, cx)?;
    loop {
        calc_skip_ws(t, p);
        match t.get(*p) {
            Some(Tok::Delim('*')) => {
                *p += 1;
                let r = calc_atom(t, p, cx)?;
                acc = match (acc, r) {
                    (CalcVal::Num(a), CalcVal::Num(b)) => CalcVal::Num(a * b),
                    (CalcVal::Len(px, pc), CalcVal::Num(n)) | (CalcVal::Num(n), CalcVal::Len(px, pc)) => CalcVal::Len(px * n, pc * n),
                    _ => return None,
                };
            }
            Some(Tok::Delim('/')) => {
                *p += 1;
                let r = calc_atom(t, p, cx)?;
                acc = match (acc, r) {
                    (CalcVal::Num(a), CalcVal::Num(b)) if b != 0.0 => CalcVal::Num(a / b),
                    (CalcVal::Len(px, pc), CalcVal::Num(n)) if n != 0.0 => CalcVal::Len(px / n, pc / n),
                    _ => return None,
                };
            }
            _ => return Some(acc),
        }
    }
}

fn calc_atom(t: &[Tok], p: &mut usize, cx: &Ctx) -> Option<CalcVal> {
    calc_skip_ws(t, p);
    let tok = t.get(*p)?.clone();
    *p += 1;
    match tok {
        Tok::Number(n) => Some(CalcVal::Num(n)),
        Tok::Percentage(n) => Some(CalcVal::Len(0.0, n)),
        Tok::Dimension(n, u) => Some(CalcVal::Len(unit_px(n, &u, cx)?, 0.0)),
        Tok::LParen | Tok::Function(_) => {
            let v = calc_sum(t, p, cx)?;
            calc_skip_ws(t, p);
            if t.get(*p) == Some(&Tok::RParen) {
                *p += 1;
            }
            Some(v)
        }
        _ => None,
    }
}

fn px_value(c: &[Tok], cx: &Ctx) -> Option<f32> {
    match length(c, cx)? {
        Len::Val(px, _) => Some(px),
        e @ Len::Expr(_) => e.resolve(0.0),
        Len::Auto => None,
    }
}

fn number(c: &[Tok]) -> Option<f32> {
    match c {
        [Tok::Number(n)] => Some(*n),
        // calc() over plain numbers, e.g. Tailwind's `z-index: calc(1 * -1)`
        [Tok::Function(f), ..] if f == "calc" => {
            let cx = Ctx { font_size: 16.0, root_font: 16.0, vp: (0.0, 0.0), parent_font: 16.0 };
            let mut pos = 0;
            match calc_sum(c, &mut pos, &cx)? {
                CalcVal::Num(n) => Some(n),
                CalcVal::Len(..) => None,
            }
        }
        _ => None,
    }
}

fn color(c: &[Tok], current: Color) -> Option<Color> {
    if let [Tok::Ident(s)] = c {
        if s.eq_ignore_ascii_case("currentcolor") {
            return Some(current);
        }
    }
    css::parse_color_with(c, Some(current))
}

fn border_style(s: &str) -> Option<BorderStyle> {
    Some(match s {
        "none" | "hidden" => BorderStyle::None,
        "solid" | "groove" | "ridge" | "inset" | "outset" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        _ => return None,
    })
}

fn border_width_kw(c: &[Tok], cx: &Ctx) -> Option<f32> {
    match ident(c).as_deref() {
        Some("thin") => Some(1.0),
        Some("medium") => Some(3.0),
        Some("thick") => Some(5.0),
        _ => px_value(c, cx),
    }
}

/// Expand 1-4 values to [top, right, bottom, left].
fn four<T: Clone>(v: &[T]) -> Option<[T; 4]> {
    Some(match v.len() {
        1 => [v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()],
        2 => [v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => [v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        4 => [v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()],
        _ => return None,
    })
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

fn side_index(name: &str) -> Option<usize> {
    SIDES.iter().position(|s| name.ends_with(s)).or_else(|| {
        if name.ends_with("block-start") {
            Some(0)
        } else if name.ends_with("inline-end") {
            Some(1)
        } else if name.ends_with("block-end") {
            Some(2)
        } else if name.ends_with("inline-start") {
            Some(3)
        } else {
            None
        }
    })
}

fn font_family(comps: &[Vec<Tok>]) -> Option<(FontFamily, String)> {
    for group in split_commas(comps) {
        let name: String = group
            .iter()
            .map(|c| match c.as_slice() {
                [Tok::Ident(s)] | [Tok::Str(s)] => s.clone(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        let fam = match name.as_str() {
            "monospace" | "courier" | "courier new" | "menlo" | "monaco" | "consolas" | "sf mono" | "ui-monospace" | "source code pro" | "fira code" | "jetbrains mono" | "lucida console" | "dejavu sans mono" | "roboto mono" => FontFamily::Mono,
            "serif" | "times" | "times new roman" | "georgia" | "garamond" | "cambria" | "palatino" | "ui-serif" | "book antiqua" => FontFamily::Serif,
            "sans-serif" | "arial" | "helvetica" | "helvetica neue" | "system-ui" | "-apple-system" | "blinkmacsystemfont" | "segoe ui" | "roboto" | "verdana" | "tahoma" | "inter" | "ui-sans-serif" | "open sans" | "noto sans" | "ubuntu" | "lato" | "trebuchet ms" | "lucida grande" => FontFamily::Sans,
            _ => continue,
        };
        return Some((fam, name));
    }
    None
}

fn font_weight(c: &[Tok], parent: u16) -> Option<u16> {
    match c {
        [Tok::Number(n)] => Some((*n as u16).clamp(1, 1000)),
        [Tok::Ident(s)] => Some(match s.to_ascii_lowercase().as_str() {
            "normal" => 400,
            "bold" => 700,
            "bolder" => (parent + 300).min(900),
            "lighter" => parent.saturating_sub(300).max(100),
            _ => return None,
        }),
        _ => None,
    }
}

fn font_size(c: &[Tok], cx: &Ctx) -> Option<f32> {
    if let Some(id) = ident(c) {
        return Some(match id.as_str() {
            "xx-small" => 9.0,
            "x-small" => 10.0,
            "small" => 13.0,
            "medium" => 16.0,
            "large" => 18.0,
            "x-large" => 24.0,
            "xx-large" => 32.0,
            "xxx-large" => 48.0,
            "smaller" => cx.parent_font / 1.2,
            "larger" => cx.parent_font * 1.2,
            _ => return None,
        });
    }
    let l = length(c, &Ctx { font_size: cx.parent_font, ..*cx })?;
    l.resolve(cx.parent_font)
}

fn line_height(c: &[Tok], cx: &Ctx, font_size: f32) -> Option<LineHeight> {
    match c {
        [Tok::Ident(s)] if s.eq_ignore_ascii_case("normal") => Some(LineHeight::Normal),
        [Tok::Number(n)] => Some(LineHeight::Mult(*n)),
        [Tok::Percentage(p)] => Some(LineHeight::Px(font_size * p / 100.0)),
        _ => {
            let l = length(c, &Ctx { font_size, ..*cx })?;
            l.resolve(font_size).map(LineHeight::Px)
        }
    }
}

fn parse_track(c: &[Tok], cx: &Ctx) -> Option<Track> {
    match c {
        [Tok::Dimension(n, u)] if u == "fr" => Some(Track::Fr(*n)),
        [Tok::Ident(s)] if s == "auto" => Some(Track::Auto),
        [Tok::Ident(s)] if s == "min-content" => Some(Track::MinContent),
        [Tok::Ident(s)] if s == "max-content" || s == "fit-content" => Some(Track::MaxContent),
        [Tok::Percentage(p)] => Some(Track::Pct(*p)),
        [Tok::Function(f), rest @ ..] if f == "minmax" => {
            let inner = &rest[..rest.len().saturating_sub(1)];
            let args = split_args(inner);
            if args.len() != 2 {
                return None;
            }
            let trim = |a: &Vec<Tok>| a.iter().filter(|t| **t != Tok::Ws).cloned().collect::<Vec<_>>();
            Some(Track::MinMax(Box::new(parse_track(&trim(&args[0]), cx)?), Box::new(parse_track(&trim(&args[1]), cx)?)))
        }
        _ => length(c, cx).map(|l| match l {
            Len::Val(px, 0.0) => Track::Px(px),
            Len::Val(0.0, pct) => Track::Pct(pct),
            other => Track::Len(other),
        }),
    }
}

fn grid_template(toks: &[Tok], cx: &Ctx) -> Option<GridTemplate> {
    let comps = components(toks);
    if ident(comps.first()?).as_deref() == Some("none") {
        return Some(GridTemplate::None);
    }
    let mut tracks = Vec::new();
    for c in comps {
        if let [Tok::Function(f), rest @ ..] = c.as_slice() {
            if f == "repeat" {
                let inner = &rest[..rest.len().saturating_sub(1)];
                let args = split_args(inner);
                if args.len() != 2 {
                    return None;
                }
                let count_toks: Vec<Tok> = args[0].iter().filter(|t| **t != Tok::Ws).cloned().collect();
                let sub = components(&args[1]);
                match count_toks.as_slice() {
                    [Tok::Number(n)] => {
                        for _ in 0..(*n as usize).min(100) {
                            for s in &sub {
                                tracks.push(parse_track(s, cx)?);
                            }
                        }
                    }
                    [Tok::Ident(kw)] if kw == "auto-fill" || kw == "auto-fit" => {
                        let t = parse_track(sub.first()?, cx)?;
                        let (min, max) = match t {
                            Track::MinMax(a, b) => (
                                match *a {
                                    Track::Px(p) => Len::px(p),
                                    Track::Pct(p) => Len::pct(p),
                                    Track::Len(l) => l,
                                    _ => Len::px(100.0),
                                },
                                *b,
                            ),
                            Track::Px(p) => (Len::px(p), Track::Px(p)),
                            Track::Pct(p) => (Len::pct(p), Track::Pct(p)),
                            Track::Len(l) => (l, Track::Len(l)),
                            _ => (Len::px(100.0), t),
                        };
                        return Some(GridTemplate::AutoFill { min, max });
                    }
                    _ => return None,
                }
                continue;
            }
        }
        if let [Tok::LBracket, ..] = c.as_slice() {
            continue; // line names
        }
        tracks.push(parse_track(&c, cx)?);
    }
    Some(GridTemplate::Tracks(tracks))
}

fn grid_line(toks: &[Tok]) -> Option<GridLine> {
    let comps = components(toks);
    let mut gl = GridLine::default();
    let parts = comps.split(|c| c.len() == 1 && c[0] == Tok::Delim('/')).collect::<Vec<_>>();
    let start = parts.first()?;
    let mut i = 0;
    while i < start.len() {
        match start[i].as_slice() {
            [Tok::Ident(s)] if s == "span" => {
                if let Some(n) = start.get(i + 1).and_then(|c| number(c)) {
                    gl.span = n.max(1.0) as u32;
                    i += 1;
                }
            }
            [Tok::Number(n)] => gl.start = Some(*n as i32),
            _ => {}
        }
        i += 1;
    }
    if let Some(end) = parts.get(1) {
        match &end[..] {
            [c] if number(c).is_some() => {
                if let Some(s) = gl.start {
                    let e = number(c).unwrap() as i32;
                    if e > s {
                        gl.span = (e - s) as u32;
                    } else if e == -1 {
                        gl.span = 0; // to end (resolved in layout)
                    }
                }
            }
            [a, b] if ident(a).as_deref() == Some("span") => {
                if let Some(n) = number(b) {
                    gl.span = n.max(1.0) as u32;
                }
            }
            _ => {}
        }
    }
    Some(gl)
}

fn parse_gradient_stops(args: &[Vec<Tok>], current: Color) -> Vec<(Color, Option<f32>)> {
    let mut stops = Vec::new();
    for a in args {
        let comps = components(a);
        let Some(first) = comps.first() else { continue };
        if let Some(c) = color(first, current) {
            let pos = comps.get(1).and_then(|p| match p.as_slice() {
                [Tok::Percentage(v)] => Some(v / 100.0),
                _ => None,
            });
            stops.push((c, pos));
        }
    }
    stops
}

fn parse_background_image(c: &[Tok], current: Color) -> Option<Background> {
    match c {
        [Tok::Url(u)] => Some(Background::Url(u.clone())),
        [Tok::Function(f), Tok::Str(u), ..] if f == "url" => Some(Background::Url(u.clone())),
        [Tok::Function(f), rest @ ..] if f.ends_with("linear-gradient") => {
            let inner = &rest[..rest.len().saturating_sub(1)];
            let mut args = split_args(inner);
            let mut angle = 180.0;
            if let Some(first) = args.first() {
                let fc = components(first);
                match fc.first().map(|c| c.as_slice()) {
                    Some([Tok::Dimension(n, u)]) if u == "deg" => {
                        angle = *n;
                        args.remove(0);
                    }
                    Some([Tok::Dimension(n, u)]) if u == "turn" => {
                        angle = n * 360.0;
                        args.remove(0);
                    }
                    Some([Tok::Ident(t)]) if t == "to" => {
                        let dirs: Vec<String> = fc[1..].iter().filter_map(|c| ident(c)).collect();
                        let has = |d: &str| dirs.iter().any(|x| x == d);
                        angle = match (has("top"), has("bottom"), has("left"), has("right")) {
                            (true, _, false, false) => 0.0,
                            (_, true, false, false) => 180.0,
                            (false, false, true, _) => 270.0,
                            (false, false, _, true) => 90.0,
                            (true, _, _, true) => 45.0,
                            (_, true, _, true) => 135.0,
                            (_, true, true, _) => 225.0,
                            (true, _, true, _) => 315.0,
                            _ => 180.0,
                        };
                        args.remove(0);
                    }
                    _ => {}
                }
            }
            let stops = parse_gradient_stops(&args, current);
            if stops.is_empty() { None } else { Some(Background::LinearGradient { angle, stops }) }
        }
        [Tok::Function(f), rest @ ..] if f.ends_with("radial-gradient") => {
            let inner = &rest[..rest.len().saturating_sub(1)];
            let mut args = split_args(inner);
            if let Some(first) = args.first() {
                if color(components(first).first().map(|v| v.as_slice()).unwrap_or(&[]), current).is_none() {
                    args.remove(0);
                }
            }
            let stops = parse_gradient_stops(&args, current);
            if stops.is_empty() { None } else { Some(Background::RadialGradient { stops }) }
        }
        _ => None,
    }
}

fn parse_repeat(k: &str) -> Option<(bool, bool)> {
    Some(match k {
        "repeat" | "round" | "space" => (true, true),
        "no-repeat" => (false, false),
        "repeat-x" => (true, false),
        "repeat-y" => (false, true),
        _ => return None,
    })
}

fn parse_bg_size(parts: &[Vec<Tok>], cx: &Ctx) -> Option<BgSizeSpec> {
    match parts {
        [] => None,
        [one] => match ident(one).as_deref() {
            Some("cover") => Some(BgSizeSpec::Cover),
            Some("contain") => Some(BgSizeSpec::Contain),
            Some("auto") => Some(BgSizeSpec::Auto),
            _ => length(one, cx).map(|w| BgSizeSpec::Explicit(w, Len::Auto)),
        },
        [a, b, ..] => Some(BgSizeSpec::Explicit(length(a, cx).unwrap_or(Len::Auto), length(b, cx).unwrap_or(Len::Auto))),
    }
}

/// Parse 1–4 value `background-position`.
fn parse_bg_pos(parts: &[Vec<Tok>], cx: &Ctx) -> Option<BgPos> {
    enum P {
        K(String),
        L(Len),
    }
    let items: Vec<P> = parts
        .iter()
        .filter_map(|c| match ident(c) {
            Some(k) if matches!(k.as_str(), "left" | "right" | "top" | "bottom" | "center") => Some(P::K(k)),
            _ => length(c, cx).map(P::L),
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let mut pos = BgPos { from_right: false, x: Len::pct(50.0), from_bottom: false, y: Len::pct(50.0) };
    let kw_len = |k: &str| match k {
        "left" | "top" => Len::ZERO,
        "right" | "bottom" => Len::pct(100.0),
        _ => Len::pct(50.0),
    };
    if items.len() <= 2 {
        // simple form: [x] [y], keywords may come in either order
        let (mut xi, mut yi): (Option<&P>, Option<&P>) = (items.first(), items.get(1));
        let is_vert = |p: Option<&P>| matches!(p, Some(P::K(k)) if k == "top" || k == "bottom");
        let is_horiz = |p: Option<&P>| matches!(p, Some(P::K(k)) if k == "left" || k == "right");
        if is_vert(xi) || is_horiz(yi) {
            std::mem::swap(&mut xi, &mut yi);
        }
        if items.len() == 1 && is_vert(yi) {
            xi = None;
        }
        if let Some(p) = xi {
            pos.x = match p {
                P::K(k) => kw_len(k),
                P::L(l) => *l,
            };
        }
        if let Some(p) = yi {
            pos.y = match p {
                P::K(k) => kw_len(k),
                P::L(l) => *l,
            };
        }
        return Some(pos);
    }
    // edge-offset form: `right 14px top 50%`
    let mut i = 0;
    while i < items.len() {
        let P::K(k) = &items[i] else {
            i += 1;
            continue;
        };
        let off = match items.get(i + 1) {
            Some(P::L(l)) => {
                i += 1;
                Some(*l)
            }
            _ => None,
        };
        match k.as_str() {
            "left" => pos.x = off.unwrap_or(Len::ZERO),
            "right" => {
                pos.from_right = true;
                pos.x = off.unwrap_or(Len::ZERO);
            }
            "top" => pos.y = off.unwrap_or(Len::ZERO),
            "bottom" => {
                pos.from_bottom = true;
                pos.y = off.unwrap_or(Len::ZERO);
            }
            _ => {}
        }
        i += 1;
    }
    Some(pos)
}

fn parse_shadows(toks: &[Tok], cx: &Ctx, current: Color) -> Vec<Shadow> {
    let comps = components(toks);
    let mut out = Vec::new();
    for group in split_commas(&comps) {
        let mut lens = Vec::new();
        let mut col = None;
        let mut inset = false;
        for c in &group {
            if ident(c).as_deref() == Some("inset") {
                inset = true;
            } else if let Some(l) = px_value(c, cx) {
                lens.push(l);
            } else if let Some(cc) = color(c, current) {
                col = Some(cc);
            }
        }
        if lens.len() >= 2 {
            out.push(Shadow {
                x: lens[0],
                y: lens[1],
                blur: lens.get(2).copied().unwrap_or(0.0),
                spread: lens.get(3).copied().unwrap_or(0.0),
                color: col.unwrap_or(Color::rgba(0, 0, 0, 64)),
                inset,
            });
        }
    }
    out
}

fn align(s: &str) -> Option<Align> {
    Some(match s {
        "auto" | "normal" => Align::Auto,
        "stretch" => Align::Stretch,
        "flex-start" | "start" | "self-start" | "left" => Align::Start,
        "flex-end" | "end" | "self-end" | "right" => Align::End,
        "center" => Align::Center,
        "baseline" | "first" | "last" => Align::Baseline,
        _ => return None,
    })
}

fn justify(s: &str) -> Option<Justify> {
    Some(match s {
        "flex-start" | "start" | "left" | "normal" | "stretch" => Justify::Start,
        "flex-end" | "end" | "right" => Justify::End,
        "center" => Justify::Center,
        "space-between" => Justify::SpaceBetween,
        "space-around" => Justify::SpaceAround,
        "space-evenly" => Justify::SpaceEvenly,
        _ => return None,
    })
}

fn overflow(s: &str) -> Option<Overflow> {
    Some(match s {
        "visible" => Overflow::Visible,
        "hidden" | "clip" => Overflow::Hidden,
        "scroll" => Overflow::Scroll,
        "auto" | "overlay" => Overflow::Auto,
        _ => return None,
    })
}

/// Copy a single property from `src` (for inherit/initial/unset).
fn copy_prop(dst: &mut ComputedStyle, src: &ComputedStyle, name: &str) {
    macro_rules! cp {
        ($($f:ident),*) => {{ $(dst.$f = src.$f.clone();)* }};
    }
    match name {
        "display" => cp!(display),
        "position" => cp!(position),
        "color" => cp!(color),
        "background" => cp!(background_color, background_images, background_sizes, background_positions, background_repeats),
        "background-color" => cp!(background_color),
        "background-image" => cp!(background_images),
        "font-size" => cp!(font_size),
        "font-weight" => cp!(font_weight),
        "font-style" => cp!(italic),
        "font-family" => cp!(font_family, font_family_name),
        "font" => cp!(font_size, font_weight, italic, font_family, font_family_name, line_height),
        "line-height" => cp!(line_height),
        "text-align" => cp!(text_align),
        "justify-items" => cp!(justify_items),
        "text-decoration" | "text-decoration-line" => cp!(underline, line_through, overline),
        "white-space" => cp!(white_space),
        "width" => cp!(width),
        "height" => cp!(height),
        "margin" => cp!(margin),
        "padding" => cp!(padding),
        "border" => cp!(border_width, border_style, border_color),
        "border-color" => cp!(border_color),
        "cursor" => cp!(cursor),
        "visibility" => cp!(visible),
        "opacity" => cp!(opacity),
        "list-style" | "list-style-type" => cp!(list_style),
        "overflow" => cp!(overflow_x, overflow_y),
        "flex" => cp!(flex_grow, flex_shrink, flex_basis),
        "pointer-events" => cp!(pointer_events),
        "letter-spacing" => cp!(letter_spacing),
        "text-transform" => cp!(text_transform),
        "word-break" | "line-break" => cp!(break_anywhere, break_sources, overflow_wrap),
        "overflow-wrap" | "word-wrap" => cp!(overflow_wrap),
        "box-shadow" => cp!(box_shadow),
        "z-index" => cp!(z_index),
        "isolation" => cp!(isolate),
        "border-radius" => cp!(border_radius),
        "border-top-left-radius" => dst.border_radius[0] = src.border_radius[0],
        "border-top-right-radius" => dst.border_radius[1] = src.border_radius[1],
        "border-bottom-right-radius" => dst.border_radius[2] = src.border_radius[2],
        "border-bottom-left-radius" => dst.border_radius[3] = src.border_radius[3],
        "vertical-align" => cp!(vertical_align),
        "border-collapse" => cp!(border_collapse),
        "border-spacing" => cp!(border_spacing),
        "top" => dst.inset[0] = src.inset[0],
        "right" => dst.inset[1] = src.inset[1],
        "bottom" => dst.inset[2] = src.inset[2],
        "left" => dst.inset[3] = src.inset[3],
        _ => {}
    }
}

/// Map a logical property name to its physical equivalent (horizontal, left-to-right).
fn physical_name(name: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    match name {
        "border-start-start-radius" => return Cow::Borrowed("border-top-left-radius"),
        "border-start-end-radius" => return Cow::Borrowed("border-top-right-radius"),
        "border-end-start-radius" => return Cow::Borrowed("border-bottom-left-radius"),
        "border-end-end-radius" => return Cow::Borrowed("border-bottom-right-radius"),
        "inset-block-start" => return Cow::Borrowed("top"),
        "inset-block-end" => return Cow::Borrowed("bottom"),
        "inset-inline-start" => return Cow::Borrowed("left"),
        "inset-inline-end" => return Cow::Borrowed("right"),
        _ => {}
    }
    if !(name.contains("-inline-") || name.contains("-block-")) || name.starts_with("--") {
        return Cow::Borrowed(name);
    }
    let n = name.replace("-inline-start", "-left").replace("-inline-end", "-right").replace("-block-start", "-top").replace("-block-end", "-bottom");
    Cow::Owned(n)
}

fn apply_decl(s: &mut ComputedStyle, parent: &ComputedStyle, name: &str, value: &[Tok], cx: &Ctx) {
    let comps = components(value);
    if comps.is_empty() {
        return;
    }
    let phys = physical_name(name);
    let name: &str = &phys;
    // two-value logical shorthands: inset-inline, inset-block, border-inline-width, ...
    if let Some([start, end]) = match name {
        "inset-inline" => Some(["left", "right"]),
        "inset-block" => Some(["top", "bottom"]),
        _ => None,
    } {
        let (a, b) = (comps[0].clone(), comps.get(1).unwrap_or(&comps[0]).clone());
        apply_decl(s, parent, start, &a, cx);
        apply_decl(s, parent, end, &b, cx);
        return;
    }
    for (logical, a, b) in [("border-inline-", "border-left-", "border-right-"), ("border-block-", "border-top-", "border-bottom-")] {
        if let Some(prop) = name.strip_prefix(logical) {
            if matches!(prop, "width" | "color" | "style") {
                let (x, y) = (comps[0].clone(), comps.get(1).unwrap_or(&comps[0]).clone());
                apply_decl(s, parent, &format!("{a}{prop}"), &x, cx);
                apply_decl(s, parent, &format!("{b}{prop}"), &y, cx);
                return;
            }
        }
    }
    let first = &comps[0];
    let kw = ident(first);
    if comps.len() == 1 {
        match kw.as_deref() {
            Some("inherit") => return copy_prop(s, parent, name),
            Some("initial") => return copy_prop(s, &ComputedStyle::default(), name),
            Some("unset") | Some("revert") | Some("revert-layer") => {
                let inherited = matches!(name, "color" | "font" | "font-size" | "font-weight" | "font-style" | "font-family" | "line-height" | "text-align" | "white-space" | "cursor" | "visibility" | "list-style" | "list-style-type" | "letter-spacing" | "text-transform" | "pointer-events" | "word-break" | "line-break" | "overflow-wrap" | "word-wrap");
                let initial = ComputedStyle::default();
                return copy_prop(s, if inherited { parent } else { &initial }, name);
            }
            _ => {}
        }
    }
    let kw = kw.unwrap_or_default();
    let cur = s.color;
    match name {
        "fill" => s.svg.fill = Some(crate::css::tokens_to_string(value).trim().to_string()),
        "stroke" => s.svg.stroke = Some(crate::css::tokens_to_string(value).trim().to_string()),
        "stroke-width" => s.svg.stroke_width = Some(crate::css::tokens_to_string(value).trim().to_string()),
        "display" => {
            let d = match kw.as_str() {
                "none" => Display::None,
                "block" | "flow-root" | "flow" => Display::Block,
                "inline" => Display::Inline,
                "inline-block" => Display::InlineBlock,
                "flex" => Display::Flex,
                "inline-flex" => Display::InlineFlex,
                "grid" => Display::Grid,
                "inline-grid" => Display::InlineGrid,
                "list-item" => Display::ListItem,
                "table" => Display::Table,
                "inline-table" => Display::InlineTable,
                "table-row-group" | "table-header-group" | "table-footer-group" => Display::TableRowGroup,
                "table-row" => Display::TableRow,
                "table-cell" => Display::TableCell,
                "table-caption" => Display::TableCaption,
                "table-column" | "table-column-group" => Display::TableColumn,
                "contents" => Display::Contents,
                _ => {
                    // two-value syntax: "block flex", "inline flex"
                    let words: Vec<String> = comps.iter().filter_map(|c| ident(c)).collect();
                    match (words.first().map(|s| s.as_str()), words.get(1).map(|s| s.as_str())) {
                        (Some("inline"), Some("flex")) => Display::InlineFlex,
                        (Some("block"), Some("flex")) => Display::Flex,
                        (Some("inline"), Some("grid")) => Display::InlineGrid,
                        (Some("block"), Some("grid")) => Display::Grid,
                        (Some("inline"), Some("flow-root")) => Display::InlineBlock,
                        _ => return,
                    }
                }
            };
            s.display = d;
        }
        "position" => {
            s.position = match kw.as_str() {
                "static" => Position::Static,
                "relative" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "sticky" | "-webkit-sticky" => Position::Sticky,
                _ => return,
            }
        }
        "float" => {
            s.float = match kw.as_str() {
                "left" | "inline-start" => Float::Left,
                "right" | "inline-end" => Float::Right,
                "none" => Float::None,
                _ => return,
            }
        }
        "clear" => s.clear = kw != "none",
        "box-sizing" => s.border_box = kw == "border-box",
        "width" | "inline-size" => {
            let kwv = match kw.as_str() {
                "fit-content" | "-webkit-fit-content" | "-moz-fit-content" => Some(SizeKw::FitContent),
                "max-content" => Some(SizeKw::MaxContent),
                "min-content" => Some(SizeKw::MinContent),
                _ => None,
            };
            if let Some(k) = kwv {
                s.width = Len::Auto;
                s.width_kw = k;
            } else if let Some(l) = length(first, cx) {
                s.width = l;
                s.width_kw = SizeKw::None;
            }
        }
        "height" | "block-size" => {
            if let Some(l) = length(first, cx).or(if kw.ends_with("content") { Some(Len::Auto) } else { None }) {
                s.height = l
            }
        }
        "min-width" | "min-inline-size" => {
            if let Some(l) = length(first, cx) {
                s.min_width = l
            }
        }
        "min-height" | "min-block-size" => {
            if let Some(l) = length(first, cx) {
                s.min_height = l
            }
        }
        "max-width" | "max-inline-size" => {
            if kw == "none" {
                s.max_width = Len::Auto
            } else if let Some(l) = length(first, cx) {
                s.max_width = l
            }
        }
        "max-height" | "max-block-size" => {
            if kw == "none" {
                s.max_height = Len::Auto
            } else if let Some(l) = length(first, cx) {
                s.max_height = l
            }
        }
        "margin" => {
            let v: Option<Vec<Len>> = comps.iter().map(|c| length(c, cx)).collect();
            if let Some(f) = v.and_then(|v| four(&v)) {
                s.margin = f;
            }
        }
        "margin-inline" | "margin-block" | "padding-inline" | "padding-block" => {
            let v: Option<Vec<Len>> = comps.iter().map(|c| length(c, cx)).collect();
            if let Some(v) = v {
                let a = v[0];
                let b = *v.get(1).unwrap_or(&a);
                let target = if name.starts_with("margin") { &mut s.margin } else { &mut s.padding };
                if name.ends_with("inline") {
                    target[3] = a;
                    target[1] = b;
                } else {
                    target[0] = a;
                    target[2] = b;
                }
            }
        }
        "padding" => {
            let v: Option<Vec<Len>> = comps.iter().map(|c| length(c, cx)).collect();
            if let Some(f) = v.and_then(|v| four(&v)) {
                s.padding = f;
            }
        }
        n if n.starts_with("margin-") => {
            if let (Some(i), Some(l)) = (side_index(n), length(first, cx)) {
                s.margin[i] = l;
            }
        }
        n if n.starts_with("padding-") => {
            if let (Some(i), Some(l)) = (side_index(n), length(first, cx)) {
                s.padding[i] = l;
            }
        }
        "top" | "right" | "bottom" | "left" => {
            if let Some(l) = length(first, cx) {
                s.inset[side_index(name).unwrap()] = l;
            }
        }
        "inset" => {
            let v: Option<Vec<Len>> = comps.iter().map(|c| length(c, cx)).collect();
            if let Some(f) = v.and_then(|v| four(&v)) {
                s.inset = f;
            }
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "outline" | "border-inline" | "border-block" => {
            let mut w = None;
            let mut st = None;
            let mut col = None;
            for c in &comps {
                if let Some(bs) = ident(c).and_then(|k| border_style(&k)) {
                    st = Some(bs);
                } else if let Some(x) = border_width_kw(c, cx) {
                    w = Some(x);
                } else if let Some(cc) = color(c, cur) {
                    col = Some(cc);
                }
            }
            if name == "outline" {
                s.outline_style = st.unwrap_or(BorderStyle::None);
                s.outline_width = w.unwrap_or(3.0);
                s.outline_color = col;
                return;
            }
            let sides: Vec<usize> = match name {
                "border" => vec![0, 1, 2, 3],
                "border-inline" => vec![1, 3],
                "border-block" => vec![0, 2],
                n => vec![side_index(n).unwrap()],
            };
            for i in sides {
                s.border_style[i] = st.unwrap_or(BorderStyle::None);
                s.border_width[i] = w.unwrap_or(3.0);
                s.border_color[i] = col;
            }
        }
        "border-width" => {
            let v: Option<Vec<f32>> = comps.iter().map(|c| border_width_kw(c, cx)).collect();
            if let Some(f) = v.and_then(|v| four(&v)) {
                s.border_width = f;
            }
        }
        "border-style" => {
            let v: Option<Vec<BorderStyle>> = comps.iter().map(|c| ident(c).and_then(|k| border_style(&k))).collect();
            if let Some(f) = v.and_then(|v| four(&v)) {
                s.border_style = f;
            }
        }
        "border-color" => {
            let v: Option<Vec<Color>> = comps.iter().map(|c| color(c, cur)).collect();
            if let Some(f) = v.and_then(|v| four(&v)) {
                s.border_color = f.map(Some);
            }
        }
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            let i = side_index(name.trim_end_matches("-width")).unwrap();
            if let Some(w) = border_width_kw(first, cx) {
                s.border_width[i] = w;
            }
        }
        "border-top-style" | "border-right-style" | "border-bottom-style" | "border-left-style" => {
            let i = side_index(name.trim_end_matches("-style")).unwrap();
            if let Some(bs) = border_style(&kw) {
                s.border_style[i] = bs;
            }
        }
        "border-top-color" | "border-right-color" | "border-bottom-color" | "border-left-color" => {
            let i = side_index(name.trim_end_matches("-color")).unwrap();
            if let Some(c) = color(first, cur) {
                s.border_color[i] = Some(c);
            }
        }
        "border-radius" => {
            let before_slash: Vec<&Vec<Tok>> = comps.iter().take_while(|c| !(c.len() == 1 && c[0] == Tok::Delim('/'))).collect();
            let v: Option<Vec<Len>> = before_slash.iter().map(|c| length(c, cx)).collect();
            if let Some(f) = v.and_then(|v| four(&v)) {
                s.border_radius = f;
            }
        }
        "border-top-left-radius" | "border-top-right-radius" | "border-bottom-right-radius" | "border-bottom-left-radius" => {
            let i = ["border-top-left-radius", "border-top-right-radius", "border-bottom-right-radius", "border-bottom-left-radius"].iter().position(|n| *n == name).unwrap();
            if let Some(l) = length(first, cx) {
                s.border_radius[i] = l;
            }
        }
        "border-collapse" => s.border_collapse = kw == "collapse",
        "border-spacing" => {
            if let Some(v) = px_value(first, cx) {
                s.border_spacing = v;
            }
        }
        "outline-width" => {
            if let Some(w) = border_width_kw(first, cx) {
                s.outline_width = w;
            }
        }
        "outline-style" => {
            if let Some(bs) = border_style(&kw) {
                s.outline_style = bs;
            }
        }
        "outline-color" => s.outline_color = color(first, cur),
        "outline-offset" => {
            if let Some(v) = px_value(first, cx) {
                s.outline_offset = v;
            }
        }
        "color" => {
            if let Some(c) = color(first, parent.color) {
                s.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = color(first, cur) {
                s.background_color = c;
            }
        }
        "background" => {
            s.background_color = Color::TRANSPARENT;
            s.background_images.clear();
            s.background_sizes.clear();
            s.background_positions.clear();
            s.background_repeats.clear();
            let layers = split_commas(&comps);
            let n = layers.len();
            for (li, layer) in layers.iter().enumerate() {
                let mut image = None;
                let mut pos_parts: Vec<Vec<Tok>> = Vec::new();
                let mut size_parts: Vec<Vec<Tok>> = Vec::new();
                let mut after_slash = false;
                let mut repeat = (true, true);
                for c in layer {
                    if c.len() == 1 && c[0] == Tok::Delim('/') {
                        after_slash = true;
                        continue;
                    }
                    if let Some(bg) = parse_background_image(c, cur) {
                        image = Some(bg);
                        continue;
                    }
                    if let Some(r) = ident(c).and_then(|k| parse_repeat(&k)) {
                        repeat = r;
                        continue;
                    }
                    if ident(c).is_some_and(|k| matches!(k.as_str(), "none" | "fixed" | "scroll" | "local" | "border-box" | "padding-box" | "content-box" | "text")) {
                        continue;
                    }
                    if li == n - 1 {
                        if let Some(col) = color(c, cur) {
                            s.background_color = col;
                            continue;
                        }
                    }
                    if after_slash {
                        size_parts.push(c.clone());
                    } else {
                        pos_parts.push(c.clone());
                    }
                }
                if let Some(img) = image {
                    s.background_images.push(img);
                    s.background_positions.push(parse_bg_pos(&pos_parts, cx).unwrap_or_default());
                    s.background_sizes.push(parse_bg_size(&size_parts, cx).unwrap_or(BgSizeSpec::Auto));
                    s.background_repeats.push(repeat);
                }
            }
        }
        "background-image" => {
            s.background_images = split_commas(&comps).iter().filter_map(|l| l.first().and_then(|c| parse_background_image(c, cur))).collect();
        }
        "background-size" => {
            s.background_sizes = split_commas(&comps).iter().map(|l| parse_bg_size(l, cx).unwrap_or(BgSizeSpec::Auto)).collect();
        }
        "background-position" => {
            s.background_positions = split_commas(&comps).iter().map(|l| parse_bg_pos(l, cx).unwrap_or_default()).collect();
        }
        "background-position-x" | "background-position-y" => {
            let parts = split_commas(&comps);
            for (i, l) in parts.iter().enumerate() {
                let v = l.first().and_then(|c| length(c, cx).or_else(|| ident(c).map(|k| match k.as_str() { "center" => Len::pct(50.0), "right" | "bottom" => Len::pct(100.0), _ => Len::ZERO })));
                let Some(v) = v else { continue };
                while s.background_positions.len() <= i {
                    s.background_positions.push(BgPos::default());
                }
                if name.ends_with('x') {
                    s.background_positions[i].x = v;
                } else {
                    s.background_positions[i].y = v;
                }
            }
        }
        "background-repeat" => {
            s.background_repeats = split_commas(&comps)
                .iter()
                .map(|l| {
                    let ks: Vec<String> = l.iter().filter_map(|c| ident(c)).collect();
                    match ks.as_slice() {
                        [a] => parse_repeat(a).unwrap_or((true, true)),
                        [a, b] => (a != "no-repeat", b != "no-repeat"),
                        _ => (true, true),
                    }
                })
                .collect();
        }
        "appearance" | "-webkit-appearance" | "-moz-appearance" => s.appearance_none = kw == "none",
        "object-fit" => {
            s.object_fit = match kw.as_str() {
                "cover" => BgSize::Cover,
                "contain" => BgSize::Contain,
                _ => BgSize::Auto,
            }
        }
        "font-size" => {
            if let Some(v) = font_size(first, cx) {
                s.font_size = v;
            }
        }
        "font-weight" => {
            if let Some(w) = font_weight(first, parent.font_weight) {
                s.font_weight = w;
            }
        }
        "font-style" => s.italic = kw == "italic" || kw == "oblique",
        "font-family" => {
            if let Some((f, n)) = font_family(&comps) {
                s.font_family = f;
                s.font_family_name = n;
            }
        }
        "font" => {
            // [style] [variant] [weight] size[/line-height] family
            let mut i = 0;
            if comps.len() == 1 && matches!(kw.as_str(), "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar") {
                s.font_family = FontFamily::Sans;
                s.font_size = 13.0;
                return;
            }
            s.italic = false;
            s.font_weight = 400;
            s.line_height = LineHeight::Normal;
            while i < comps.len() {
                let c = &comps[i];
                let k = ident(c).unwrap_or_default();
                if k == "italic" || k == "oblique" {
                    s.italic = true;
                } else if k == "normal" || k == "small-caps" {
                } else if let Some(w) = font_weight(c, parent.font_weight).filter(|_| number(c).is_none() || comps.len() > i + 2) {
                    s.font_weight = w;
                } else if let Some(sz) = font_size(c, cx) {
                    s.font_size = sz;
                    if comps.get(i + 1).is_some_and(|c| c.len() == 1 && c[0] == Tok::Delim('/')) {
                        if let Some(lh) = comps.get(i + 2).and_then(|c| line_height(c, cx, sz)) {
                            s.line_height = lh;
                        }
                        i += 2;
                    }
                    if let Some((f, n)) = font_family(&comps[i + 1..]) {
                        s.font_family = f;
                        s.font_family_name = n;
                    }
                    break;
                }
                i += 1;
            }
        }
        "line-height" => {
            if let Some(lh) = line_height(first, cx, s.font_size) {
                s.line_height = lh;
            }
        }
        "text-align" => {
            s.text_align = match kw.as_str() {
                "left" | "start" | "-webkit-left" => TextAlign::Left,
                "right" | "end" | "-webkit-right" => TextAlign::Right,
                "center" => TextAlign::Center,
                "-webkit-center" => TextAlign::WebkitCenter,
                "justify" => TextAlign::Justify,
                _ => return,
            }
        }
        "text-decoration" | "text-decoration-line" => {
            s.underline = false;
            s.line_through = false;
            s.overline = false;
            for c in &comps {
                match ident(c).as_deref() {
                    Some("underline") => s.underline = true,
                    Some("line-through") => s.line_through = true,
                    Some("overline") => s.overline = true,
                    _ => {
                        if let Some(col) = color(c, cur) {
                            s.decoration_color = Some(col);
                        }
                    }
                }
            }
        }
        "text-decoration-color" => s.decoration_color = color(first, cur),
        "text-transform" => {
            s.text_transform = match kw.as_str() {
                "uppercase" => TextTransform::Upper,
                "lowercase" => TextTransform::Lower,
                "capitalize" => TextTransform::Capitalize,
                _ => TextTransform::None,
            }
        }
        "white-space" | "white-space-collapse" | "text-wrap" | "text-wrap-mode" => {
            s.white_space = match kw.as_str() {
                "normal" | "wrap" => WhiteSpace::Normal,
                "pre" | "preserve" => WhiteSpace::Pre,
                "nowrap" => WhiteSpace::NoWrap,
                "pre-wrap" | "break-spaces" => WhiteSpace::PreWrap,
                "pre-line" => WhiteSpace::PreLine,
                _ => return,
            }
        }
        "letter-spacing" => s.letter_spacing = if kw == "normal" { 0.0 } else { px_value(first, cx).unwrap_or(0.0) },
        "word-spacing" => s.word_spacing = if kw == "normal" { 0.0 } else { px_value(first, cx).unwrap_or(0.0) },
        "text-indent" => {
            if let Some(l) = length(first, cx) {
                s.text_indent = l;
            }
        }
        "text-overflow" => s.ellipsis = kw == "ellipsis",
        "word-break" | "line-break" => {
            // bit 1: word-break: break-all; bit 2: line-break: anywhere
            let bit = if name == "word-break" { 1 } else { 2 };
            let on = matches!((name, kw.as_str()), ("word-break", "break-all") | ("line-break", "anywhere"));
            s.break_sources = if on { s.break_sources | bit } else { s.break_sources & !bit };
            s.break_anywhere = s.break_sources != 0;
            // legacy `word-break: break-word` behaves like `overflow-wrap: anywhere`
            if name == "word-break" && kw == "break-word" {
                s.overflow_wrap = 2;
            }
        }
        "overflow-wrap" | "word-wrap" => {
            s.overflow_wrap = match kw.as_str() {
                "anywhere" => 2,
                "break-word" => 1,
                _ => 0,
            }
        }
        "vertical-align" => {
            s.vertical_align = match kw.as_str() {
                "middle" => VAlign::Middle,
                "top" => VAlign::Top,
                "bottom" => VAlign::Bottom,
                "text-top" => VAlign::TextTop,
                "text-bottom" => VAlign::TextBottom,
                "sub" => VAlign::Sub,
                "super" => VAlign::Super,
                _ => VAlign::Baseline,
            }
        }
        "visibility" => s.visible = kw == "visible",
        "opacity" => {
            if let Some(n) = number(first) {
                s.opacity = n.clamp(0.0, 1.0);
            } else if let [Tok::Percentage(p)] = first.as_slice() {
                s.opacity = (p / 100.0).clamp(0.0, 1.0);
            }
        }
        "overflow" => {
            if let Some(o) = overflow(&kw) {
                s.overflow_x = o;
                s.overflow_y = comps.get(1).and_then(|c| ident(c)).and_then(|k| overflow(&k)).unwrap_or(o);
            }
        }
        "overflow-x" => {
            if let Some(o) = overflow(&kw) {
                s.overflow_x = o;
            }
        }
        "overflow-y" => {
            if let Some(o) = overflow(&kw) {
                s.overflow_y = o;
            }
        }
        "z-index" => s.z_index = number(first).map(|n| n as i32),
        "isolation" => s.isolate = kw == "isolate",
        "cursor" => {
            // take the first keyword we understand (skip url() fallbacks)
            for c in &comps {
                if let Some(k) = ident(c) {
                    s.cursor = match k.as_str() {
                        "pointer" | "hand" => Cursor::Pointer,
                        "text" | "vertical-text" => Cursor::Text,
                        "move" | "all-scroll" => Cursor::Move,
                        "not-allowed" | "no-drop" => Cursor::NotAllowed,
                        "grab" | "grabbing" => Cursor::Grab,
                        "crosshair" => Cursor::Crosshair,
                        "ew-resize" | "col-resize" | "e-resize" | "w-resize" => Cursor::ResizeEw,
                        "ns-resize" | "row-resize" | "n-resize" | "s-resize" => Cursor::ResizeNs,
                        _ => Cursor::Default,
                    };
                    break;
                }
            }
        }
        "list-style-type" | "list-style" => {
            for c in &comps {
                match ident(c).as_deref() {
                    Some("none") => s.list_style = ListStyle::None,
                    Some("disc") => s.list_style = ListStyle::Disc,
                    Some("circle") => s.list_style = ListStyle::Circle,
                    Some("square") => s.list_style = ListStyle::Square,
                    Some("decimal") | Some("decimal-leading-zero") => s.list_style = ListStyle::Decimal,
                    Some("lower-alpha") | Some("lower-latin") => s.list_style = ListStyle::LowerAlpha,
                    Some("upper-alpha") | Some("upper-latin") => s.list_style = ListStyle::UpperAlpha,
                    Some("lower-roman") => s.list_style = ListStyle::LowerRoman,
                    Some("upper-roman") => s.list_style = ListStyle::UpperRoman,
                    Some("disclosure-open") => s.list_style = ListStyle::DisclosureOpen,
                    Some("disclosure-closed") => s.list_style = ListStyle::DisclosureClosed,
                    Some("inside") => s.list_inside = true,
                    Some("outside") => s.list_inside = false,
                    _ => {}
                }
            }
        }
        "list-style-position" => s.list_inside = kw == "inside",
        "flex-direction" => {
            s.flex_direction = match kw.as_str() {
                "row" => FlexDirection::Row,
                "row-reverse" => FlexDirection::RowReverse,
                "column" => FlexDirection::Column,
                "column-reverse" => FlexDirection::ColumnReverse,
                _ => return,
            }
        }
        "flex-wrap" => s.flex_wrap = kw == "wrap" || kw == "wrap-reverse",
        "flex-flow" => {
            for c in &comps {
                match ident(c).as_deref() {
                    Some("row") => s.flex_direction = FlexDirection::Row,
                    Some("row-reverse") => s.flex_direction = FlexDirection::RowReverse,
                    Some("column") => s.flex_direction = FlexDirection::Column,
                    Some("column-reverse") => s.flex_direction = FlexDirection::ColumnReverse,
                    Some("wrap") | Some("wrap-reverse") => s.flex_wrap = true,
                    Some("nowrap") => s.flex_wrap = false,
                    _ => {}
                }
            }
        }
        "justify-content" => {
            let k = comps.iter().filter_map(|c| ident(c)).find(|k| justify(k).is_some()).unwrap_or(kw.clone());
            if let Some(j) = justify(&k) {
                s.justify_content = j;
            }
        }
        "align-items" => {
            if let Some(a) = align(&kw) {
                s.align_items = if a == Align::Auto { Align::Stretch } else { a };
            }
        }
        "align-self" => {
            if let Some(a) = align(&kw) {
                s.align_self = a;
            }
        }
        "align-content" => {
            if let Some(a) = align(&kw) {
                s.align_content = a;
            } else if kw == "space-between" || kw == "space-around" || kw == "space-evenly" {
                s.align_content = Align::Start;
            }
        }
        "place-items" => {
            if let Some(a) = align(&kw) {
                s.align_items = if a == Align::Auto { Align::Stretch } else { a };
                let j = comps.get(1).and_then(|c| ident(c)).and_then(|k| align(&k)).unwrap_or(a);
                s.justify_items = j;
            }
        }
        "place-self" => {
            if let Some(a) = align(&kw) {
                s.align_self = a;
                s.justify_self = comps.get(1).and_then(|c| ident(c)).and_then(|k| align(&k)).unwrap_or(a);
            }
        }
        "justify-items" => {
            let k = comps.iter().filter_map(|c| ident(c)).find(|k| align(k).is_some()).unwrap_or(kw.clone());
            if let Some(a) = align(&k) {
                s.justify_items = a;
            }
        }
        "justify-self" => {
            if let Some(a) = align(&kw) {
                s.justify_self = a;
            }
        }
        "place-content" => {
            if let Some(a) = align(&kw) {
                s.align_content = a;
            }
            let jk = comps.get(1).and_then(|c| ident(c)).unwrap_or(kw.clone());
            if let Some(j) = justify(&jk) {
                s.justify_content = j;
            }
        }
        "flex-grow" => s.flex_grow = number(first).unwrap_or(0.0),
        "flex-shrink" => s.flex_shrink = number(first).unwrap_or(1.0),
        "flex-basis" => {
            if kw == "content" {
                s.flex_basis = Len::Auto;
            } else if let Some(l) = length(first, cx) {
                s.flex_basis = l;
            }
        }
        "flex" => match kw.as_str() {
            "none" => {
                s.flex_grow = 0.0;
                s.flex_shrink = 0.0;
                s.flex_basis = Len::Auto;
            }
            "auto" => {
                s.flex_grow = 1.0;
                s.flex_shrink = 1.0;
                s.flex_basis = Len::Auto;
            }
            _ => {
                let mut nums = Vec::new();
                let mut basis = None;
                for c in &comps {
                    if let Some(n) = number(c) {
                        nums.push(n);
                    } else if let Some(l) = length(c, cx) {
                        basis = Some(l);
                    }
                }
                s.flex_grow = nums.first().copied().unwrap_or(1.0);
                s.flex_shrink = nums.get(1).copied().unwrap_or(1.0);
                s.flex_basis = basis.unwrap_or(if nums.is_empty() { Len::Auto } else { Len::ZERO });
            }
        },
        "order" => s.order = number(first).unwrap_or(0.0) as i32,
        "gap" | "grid-gap" => {
            let r = px_value(first, cx).unwrap_or(0.0);
            s.row_gap = r;
            s.column_gap = comps.get(1).and_then(|c| px_value(c, cx)).unwrap_or(r);
        }
        "row-gap" | "grid-row-gap" => s.row_gap = px_value(first, cx).unwrap_or(0.0),
        "column-gap" | "grid-column-gap" => s.column_gap = px_value(first, cx).unwrap_or(0.0),
        "grid-template-columns" => {
            if let Some(t) = grid_template(value, cx) {
                s.grid_columns = t;
            }
        }
        "grid-template-rows" => {
            if let Some(t) = grid_template(value, cx) {
                s.grid_rows = t;
            }
        }
        "grid-column" => {
            if let Some(g) = grid_line(value) {
                s.grid_column = g;
            }
        }
        "grid-row" => {
            if let Some(g) = grid_line(value) {
                s.grid_row = g;
            }
        }
        "grid-column-start" => {
            if let Some(g) = grid_line(value) {
                s.grid_column.start = g.start;
                if g.span > 1 {
                    s.grid_column.span = g.span;
                }
            }
        }
        "grid-row-start" => {
            if let Some(g) = grid_line(value) {
                s.grid_row.start = g.start;
                if g.span > 1 {
                    s.grid_row.span = g.span;
                }
            }
        }
        "box-shadow" => s.box_shadow = if kw == "none" { vec![] } else { parse_shadows(value, cx, cur) },
        "content" => {
            s.content = if kw == "none" || kw == "normal" { None } else { Some(css::tokens_to_string(value)) };
        }
        "pointer-events" => s.pointer_events = kw != "none",
        "transform" => {
            s.translate = (Len::ZERO, Len::ZERO);
            s.scale = 1.0;
            for c in &comps {
                if let [Tok::Function(f), rest @ ..] = c.as_slice() {
                    let inner = &rest[..rest.len().saturating_sub(1)];
                    let args: Vec<Vec<Tok>> = split_args(inner).into_iter().map(|a| a.into_iter().filter(|t| *t != Tok::Ws).collect()).collect();
                    let l = |i: usize| args.get(i).and_then(|a| length(a, cx)).unwrap_or(Len::ZERO);
                    match f.as_str() {
                        "translate" => s.translate = (l(0), l(1)),
                        "translatex" => s.translate.0 = l(0),
                        "translatey" => s.translate.1 = l(0),
                        "translate3d" => s.translate = (l(0), l(1)),
                        "scale" => s.scale = args.first().and_then(|a| number(a)).unwrap_or(1.0),
                        _ => {}
                    }
                }
            }
        }
        "translate" => {
            let a = length(first, cx).unwrap_or(Len::ZERO);
            let b = comps.get(1).and_then(|c| length(c, cx)).unwrap_or(Len::ZERO);
            s.translate = (a, b);
        }
        "aspect-ratio" => {
            let nums: Vec<f32> = comps.iter().filter_map(|c| number(c)).collect();
            s.aspect_ratio = match nums.as_slice() {
                [a, b] if *b != 0.0 => Some(a / b),
                [a] => Some(*a),
                _ => None,
            };
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::parse_document;

    fn styles_for(html: &str, css: &str) -> (Document, Styles) {
        let doc = parse_document(html);
        let sheets = vec![(Origin::UserAgent, Rc::new(css::parse_stylesheet(UA_CSS))), (Origin::Author, Rc::new(css::parse_stylesheet(css)))];
        let rules = RuleSet::new(&sheets);
        let ctx = StyleContext { doc: &doc, rules: &rules, state: MatchState::default(), viewport: (800.0, 600.0) };
        let st = compute_styles(&ctx);
        (doc, st)
    }

    #[test]
    fn cascade_and_inherit() {
        let (doc, st) = styles_for(r#"<div id=a class=c style="color: blue"><span>x</span></div>"#, "#a { color: red; font-size: 20px } .c { margin: 1em 2px }");
        let a = doc.get_element_by_id("a").unwrap();
        let s = st.get(a).unwrap();
        assert_eq!(s.color, Color::rgb(0, 0, 255));
        assert_eq!(s.margin[0], Len::px(20.0));
        assert_eq!(s.margin[1], Len::px(2.0));
        let span = doc.elements_by_tag(0, "span")[0];
        assert_eq!(st.get(span).unwrap().font_size, 20.0);
        assert_eq!(st.get(span).unwrap().color, Color::rgb(0, 0, 255));
    }

    #[test]
    fn vars_and_calc() {
        let (doc, st) = styles_for(r#"<div id=a></div>"#, ":root { --w: 10px; --c: #0f0 } #a { width: calc(100% - var(--w) * 2); color: var(--c); }");
        let a = doc.get_element_by_id("a").unwrap();
        let s = st.get(a).unwrap();
        assert_eq!(s.width, Len::Val(-20.0, 100.0));
        assert_eq!(s.color, Color::rgb(0, 255, 0));
    }

    #[test]
    fn background_layers() {
        let (doc, st) = styles_for(
            r#"<select id=a></select>"#,
            "#a { appearance: none; background-image: linear-gradient(45deg, transparent 50%, red 50%), linear-gradient(135deg, red 50%, transparent 50%); background-position: right 14px top 50%, right 9px top 50%; background-size: 5px 5px, 5px 5px; background-repeat: no-repeat; }",
        );
        let a = doc.get_element_by_id("a").unwrap();
        let s = st.get(a).unwrap();
        assert!(s.appearance_none);
        assert_eq!(s.background_images.len(), 2);
        assert_eq!(s.background_positions[0], BgPos { from_right: true, x: Len::px(14.0), from_bottom: false, y: Len::pct(50.0) });
        assert_eq!(s.background_sizes[1], BgSizeSpec::Explicit(Len::px(5.0), Len::px(5.0)));
        assert_eq!(s.background_repeats[0], (false, false));
        let (doc, st) = styles_for(r#"<div id=b></div>"#, "#b { background: url(x.png) center / cover no-repeat, #123456; }");
        let s = st.get(doc.get_element_by_id("b").unwrap()).unwrap();
        assert_eq!(s.background_sizes[0], BgSizeSpec::Cover);
        assert_eq!(s.background_positions[0].x, Len::pct(50.0));
        assert_eq!(s.background_color, Color::rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn pseudo_before() {
        let (doc, st) = styles_for(r#"<p id=a title=hi>x</p>"#, r#"#a::before { content: "[" attr(title) "]"; color: red }"#);
        let a = doc.get_element_by_id("a").unwrap();
        assert_eq!(st.before.get(&a).unwrap().content.as_deref(), Some("[hi]"));
    }
}
