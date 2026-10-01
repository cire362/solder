//! Entity-relationship diagrams: tables as boxes, foreign keys as lines.
//! The model, the automatic layout and the line routing live here without
//! UI, so the editor's canvas and the SVG, PNG and Mermaid exports draw the
//! same diagram.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{Engine, ObjectKind, Schema, sql};

pub const HEADER: f32 = 30.;
pub const ROW: f32 = 22.;
/// Advance of one character of the diagram's monospace text.
pub const CHAR: f32 = 7.2;
const MIN_WIDTH: f32 = 160.;
const MAX_WIDTH: f32 = 380.;
const COLUMN_GAP: f32 = 120.;
const ROW_GAP: f32 = 40.;

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub type_name: String,
    pub key: bool,
    /// Part of a foreign key.
    pub reference: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub namespace: Option<String>,
    pub name: String,
    pub view: bool,
    pub fields: Vec<Field>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

impl Node {
    /// `schema.table`, the key its position is saved under.
    pub fn id(&self) -> String {
        match &self.namespace {
            Some(ns) => format!("{ns}.{}", self.name),
            None => self.name.clone(),
        }
    }

    pub fn height(&self) -> f32 {
        HEADER + ROW * self.fields.len() as f32
    }

    /// Vertical middle of a field's row.
    pub fn row_y(&self, field: &str) -> f32 {
        let ix = self
            .fields
            .iter()
            .position(|f| f.name == field)
            .unwrap_or(0);
        self.y + HEADER + ROW * ix as f32 + ROW / 2.
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height()
    }

    /// The field under a point inside the box.
    pub fn field_at(&self, y: f32) -> Option<&Field> {
        let row = ((y - self.y - HEADER) / ROW).floor();
        (row >= 0.).then(|| self.fields.get(row as usize)).flatten()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub name: String,
    /// The table with the foreign key.
    pub from: usize,
    pub from_columns: Vec<String>,
    /// The table it points at.
    pub to: usize,
    pub to_columns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diagram {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Saved positions, by table id. Lives in `.solder/erd/<connection>.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub tables: BTreeMap<String, Position>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

impl Layout {
    pub fn load(path: &Path) -> Layout {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")
    }

    pub fn of(diagram: &Diagram) -> Layout {
        Layout {
            tables: diagram
                .nodes
                .iter()
                .map(|n| (n.id(), Position { x: n.x, y: n.y }))
                .collect(),
        }
    }
}

/// The file a connection's layout is saved in.
pub fn layout_path(root: &Path, connection: &str) -> std::path::PathBuf {
    let safe: String = connection
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    root.join(".solder/erd").join(format!("{safe}.json"))
}

/// The tables and views of `schema` with their relations. Saved positions
/// are kept; tables without one are laid out automatically.
pub fn diagram(schema: &Schema, saved: &Layout) -> Diagram {
    let mut nodes: Vec<Node> = schema
        .objects
        .iter()
        .filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View))
        .map(|o| {
            let referencing: Vec<&str> = o
                .foreign_keys
                .iter()
                .flat_map(|fk| fk.columns.iter().map(String::as_str))
                .collect();
            let fields: Vec<Field> = o
                .columns
                .iter()
                .map(|c| Field {
                    name: c.name.clone(),
                    type_name: c.type_name.clone(),
                    key: c.primary_key,
                    reference: referencing.contains(&c.name.as_str()),
                })
                .collect();
            let widest = fields
                .iter()
                .map(|f| f.name.chars().count() + f.type_name.chars().count() + 6)
                .chain([o.name.chars().count() + 4])
                .max()
                .unwrap_or(0);
            Node {
                namespace: o.namespace.clone(),
                name: o.name.clone(),
                view: o.kind == ObjectKind::View,
                fields,
                x: 0.,
                y: 0.,
                width: (widest as f32 * CHAR + 24.).clamp(MIN_WIDTH, MAX_WIDTH),
            }
        })
        .collect();
    let index: HashMap<(Option<String>, String), usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| ((n.namespace.clone(), n.name.clone()), i))
        .collect();
    let find = |ns: &Option<String>, name: &str| {
        index
            .get(&(ns.clone(), name.to_string()))
            .or_else(|| index.iter().find(|((_, n), _)| n == name).map(|(_, i)| i))
            .copied()
    };
    let mut edges = Vec::new();
    for (from, o) in schema
        .objects
        .iter()
        .filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View))
        .enumerate()
    {
        for fk in &o.foreign_keys {
            let namespace = fk.ref_namespace.clone().or_else(|| o.namespace.clone());
            if let Some(to) = find(&namespace, &fk.ref_table) {
                edges.push(Edge {
                    name: fk.name.clone(),
                    from,
                    from_columns: fk.columns.clone(),
                    to,
                    to_columns: fk.ref_columns.clone(),
                });
            }
        }
    }
    let mut diagram = Diagram {
        nodes: std::mem::take(&mut nodes),
        edges,
    };
    auto_layout(&mut diagram);
    for node in &mut diagram.nodes {
        if let Some(p) = saved.tables.get(&node.id()) {
            node.x = p.x;
            node.y = p.y;
        }
    }
    diagram
}

/// Referenced tables to the left, the tables pointing at them to their
/// right, one column per level; rows ordered to keep related tables close.
/// Tables with no relations go in a grid after the related ones.
pub fn auto_layout(diagram: &mut Diagram) {
    let n = diagram.nodes.len();
    let related: Vec<bool> = (0..n)
        .map(|i| {
            diagram
                .edges
                .iter()
                .any(|e| e.from != e.to && (e.from == i || e.to == i))
        })
        .collect();
    // Level = longest chain of references below it. Bounded passes keep
    // cycles from looping forever.
    let mut level = vec![0usize; n];
    for _ in 0..n {
        let mut changed = false;
        for e in &diagram.edges {
            if e.from != e.to && level[e.from] < level[e.to] + 1 && level[e.to] + 1 < n {
                level[e.from] = level[e.to] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let levels = (0..n)
        .filter(|&i| related[i])
        .map(|i| level[i])
        .max()
        .map_or(0, |m| m + 1);
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); levels];
    for i in (0..n).filter(|&i| related[i]) {
        columns[level[i]].push(i);
    }
    for column in &mut columns {
        column.sort_by(|a, b| diagram.nodes[*a].name.cmp(&diagram.nodes[*b].name));
    }
    // A few sweeps placing each table near the average row of its neighbors.
    for _ in 0..4 {
        let rank: HashMap<usize, usize> = columns
            .iter()
            .flat_map(|c| c.iter().enumerate().map(|(r, &i)| (i, r)))
            .collect();
        for column in &mut columns {
            let mut keyed: Vec<(f32, usize)> = column
                .iter()
                .map(|&i| {
                    let neighbors: Vec<usize> = diagram
                        .edges
                        .iter()
                        .filter_map(|e| match (e.from == i, e.to == i) {
                            (true, false) => Some(e.to),
                            (false, true) => Some(e.from),
                            _ => None,
                        })
                        .collect();
                    let average = if neighbors.is_empty() {
                        rank[&i] as f32
                    } else {
                        neighbors.iter().map(|j| rank[j] as f32).sum::<f32>()
                            / neighbors.len() as f32
                    };
                    (average, i)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            *column = keyed.into_iter().map(|(_, i)| i).collect();
        }
    }
    let mut x = 40.;
    for column in &columns {
        let mut y = 40.;
        let mut width: f32 = 0.;
        for &i in column {
            let node = &mut diagram.nodes[i];
            node.x = x;
            node.y = y;
            y += node.height() + ROW_GAP;
            width = width.max(node.width);
        }
        x += width + COLUMN_GAP;
    }
    // Unrelated tables: a grid of four columns.
    let lonely: Vec<usize> = (0..n).filter(|&i| !related[i]).collect();
    let mut row_y = 40.;
    for row in lonely.chunks(4) {
        let mut cx = x;
        let mut tallest: f32 = 0.;
        for &i in row {
            let node = &mut diagram.nodes[i];
            node.x = cx;
            node.y = row_y;
            cx += node.width + 60.;
            tallest = tallest.max(node.height());
        }
        row_y += tallest + ROW_GAP;
    }
}

/// The line of a relation: from the referencing column's row to the
/// referenced column's row, leaving and entering boxes sideways.
pub fn route(diagram: &Diagram, edge: &Edge) -> Vec<(f32, f32)> {
    let from = &diagram.nodes[edge.from];
    let to = &diagram.nodes[edge.to];
    let y0 = from.row_y(edge.from_columns.first().map_or("", String::as_str));
    let y1 = to.row_y(edge.to_columns.first().map_or("", String::as_str));
    if edge.from == edge.to {
        let right = from.x + from.width;
        return vec![
            (right, y0),
            (right + 30., y0),
            (right + 30., y1),
            (right, y1),
        ];
    }
    let sideways = |x0: f32, x1: f32| {
        // The bend goes next to the source, midway or next to the target:
        // whichever crosses the fewest other tables.
        let step = if x1 > x0 { 30. } else { -30. };
        [x0 + step, (x0 + x1) / 2., x1 - step]
            .into_iter()
            .map(|mid| vec![(x0, y0), (mid, y0), (mid, y1), (x1, y1)])
            .min_by_key(|points| crossings(diagram, edge, points))
            .unwrap_or_default()
    };
    if to.x > from.x + from.width + 20. {
        sideways(from.x + from.width, to.x)
    } else if to.x + to.width + 20. < from.x {
        sideways(from.x, to.x + to.width)
    } else {
        // Overlapping columns: go around on the right.
        let out = (from.x + from.width).max(to.x + to.width) + 30.;
        vec![
            (from.x + from.width, y0),
            (out, y0),
            (out, y1),
            (to.x + to.width, y1),
        ]
    }
}

/// How many tables other than the edge's own a route runs through.
fn crossings(diagram: &Diagram, edge: &Edge, points: &[(f32, f32)]) -> usize {
    diagram
        .nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != edge.from && *i != edge.to)
        .filter(|(_, n)| {
            points.windows(2).any(|w| {
                let ((ax, ay), (bx, by)) = (w[0], w[1]);
                let (left, right) = (ax.min(bx), ax.max(bx));
                let (top, bottom) = (ay.min(by), ay.max(by));
                left < n.x + n.width && right > n.x && top < n.y + n.height() && bottom > n.y
            })
        })
        .count()
}

/// Everything drawn, for sizing exports.
pub fn bounds(diagram: &Diagram) -> (f32, f32, f32, f32) {
    let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for n in &diagram.nodes {
        b.0 = b.0.min(n.x);
        b.1 = b.1.min(n.y);
        b.2 = b.2.max(n.x + n.width + 40.);
        b.3 = b.3.max(n.y + n.height());
    }
    if diagram.nodes.is_empty() {
        (0., 0., 100., 100.)
    } else {
        b
    }
}

/// A query joining the two tables of a relation.
pub fn join_query(engine: Engine, diagram: &Diagram, edge: &Edge) -> String {
    let from = &diagram.nodes[edge.from];
    let to = &diagram.nodes[edge.to];
    let a = sql::qualified_name(engine, from.namespace.as_deref(), &from.name);
    let b = sql::qualified_name(engine, to.namespace.as_deref(), &to.name);
    let on: Vec<String> = edge
        .from_columns
        .iter()
        .zip(&edge.to_columns)
        .map(|(c, r)| {
            format!(
                "c.{} = p.{}",
                sql::quote_ident(engine, c),
                sql::quote_ident(engine, r)
            )
        })
        .collect();
    format!(
        "SELECT * FROM {a} c JOIN {b} p ON {} LIMIT 200",
        on.join(" AND ")
    )
}

/// Mermaid `erDiagram` source, for docs and pull requests.
pub fn mermaid(diagram: &Diagram) -> String {
    let word = |s: &str| -> String {
        let w: String = s
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let w = w.trim_matches('_').to_string();
        if w.is_empty() { "_".into() } else { w }
    };
    let mut out = String::from("erDiagram\n");
    for n in &diagram.nodes {
        out.push_str(&format!("  {} {{\n", word(&n.name)));
        for f in &n.fields {
            let mut keys = Vec::new();
            if f.key {
                keys.push("PK");
            }
            if f.reference {
                keys.push("FK");
            }
            out.push_str(&format!(
                "    {} {}{}\n",
                word(&f.type_name),
                word(&f.name),
                if keys.is_empty() {
                    String::new()
                } else {
                    format!(" {}", keys.join(", "))
                }
            ));
        }
        out.push_str("  }\n");
    }
    for e in &diagram.edges {
        out.push_str(&format!(
            "  {} ||--o{{ {} : \"{}\"\n",
            word(&diagram.nodes[e.to].name),
            word(&diagram.nodes[e.from].name),
            e.from_columns.join(", ")
        ));
    }
    out
}

/// Colors for exports: the editor's dark theme by default.
#[derive(Clone, Debug)]
pub struct Palette {
    pub background: String,
    pub panel: String,
    pub header: String,
    pub line: String,
    pub text: String,
    pub muted: String,
    pub accent: String,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            background: "#0c0c0e".into(),
            panel: "#141417".into(),
            header: "#1c1c21".into(),
            line: "#3f3f46".into(),
            text: "#ededef".into(),
            muted: "#71717a".into(),
            accent: "#e8743f".into(),
        }
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The diagram as a standalone SVG.
pub fn svg(diagram: &Diagram, palette: &Palette) -> String {
    let (x0, y0, x1, y1) = bounds(diagram);
    let (x0, y0) = (x0 - 20., y0 - 20.);
    let (w, h) = (x1 - x0 + 20., y1 - y0 + 20.);
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" viewBox=\"{x0:.0} {y0:.0} {w:.0} {h:.0}\" \
         font-family=\"Menlo, Consolas, monospace\" font-size=\"12\">\n\
         <rect x=\"{x0:.0}\" y=\"{y0:.0}\" width=\"{w:.0}\" height=\"{h:.0}\" fill=\"{}\"/>\n",
        palette.background
    );
    for e in &diagram.edges {
        let points: Vec<String> = route(diagram, e)
            .iter()
            .map(|(x, y)| format!("{x:.1},{y:.1}"))
            .collect();
        out.push_str(&format!(
            "<polyline points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"1.5\"/>\n",
            points.join(" "),
            palette.muted
        ));
        if let (Some(&(sx, sy)), Some(&(ex, ey))) =
            (route(diagram, e).first(), route(diagram, e).last())
        {
            out.push_str(&format!(
                "<circle cx=\"{sx:.1}\" cy=\"{sy:.1}\" r=\"3\" fill=\"{}\"/>\n",
                palette.accent
            ));
            out.push_str(&format!(
                "<line x1=\"{ex:.1}\" y1=\"{:.1}\" x2=\"{ex:.1}\" y2=\"{:.1}\" stroke=\"{}\" stroke-width=\"2\"/>\n",
                ey - 6.,
                ey + 6.,
                palette.accent
            ));
        }
    }
    for n in &diagram.nodes {
        out.push_str(&format!(
            "<g><rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"8\" fill=\"{}\" stroke=\"{}\"/>\n",
            n.x, n.y, n.width, n.height(), palette.panel, palette.line
        ));
        out.push_str(&format!(
            "<path d=\"M{x:.1},{y2:.1} v-{r:.1} a8,8 0 0 1 8,-8 h{w8:.1} a8,8 0 0 1 8,8 v{r:.1} z\" fill=\"{}\"/>\n",
            palette.header,
            x = n.x,
            y2 = n.y + HEADER,
            r = HEADER - 8.,
            w8 = n.width - 16.,
        ));
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\" font-weight=\"bold\">{}{}</text>\n",
            n.x + 12.,
            n.y + 19.,
            palette.text,
            escape(&n.name),
            if n.view { " (view)" } else { "" }
        ));
        for (i, f) in n.fields.iter().enumerate() {
            let y = n.y + HEADER + ROW * i as f32 + 15.;
            let color = if f.key {
                &palette.accent
            } else {
                &palette.text
            };
            out.push_str(&format!(
                "<text x=\"{:.1}\" y=\"{y:.1}\" fill=\"{color}\">{}{}</text>\n",
                n.x + 12.,
                escape(&f.name),
                if f.reference { " →" } else { "" }
            ));
            out.push_str(&format!(
                "<text x=\"{:.1}\" y=\"{y:.1}\" fill=\"{}\" text-anchor=\"end\">{}</text>\n",
                n.x + n.width - 12.,
                palette.muted,
                escape(&f.type_name)
            ));
        }
        out.push_str("</g>\n");
    }
    out.push_str("</svg>\n");
    out
}

/// The SVG drawn at `scale` into a PNG, with the system's fonts.
pub fn png(svg: &str, scale: f32) -> Result<Vec<u8>, String> {
    use resvg::{tiny_skia, usvg};
    let mut options = usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_str(svg, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    let (w, h) = (
        (size.width() * scale).ceil() as u32,
        (size.height() * scale).ceil() as u32,
    );
    let mut pixmap =
        tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or("The diagram is too large for a PNG")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColumnInfo, ForeignKey, Object};

    fn schema() -> Schema {
        let table = |name: &str, columns: &[(&str, bool)], fks: Vec<ForeignKey>| Object {
            namespace: Some("public".into()),
            name: name.into(),
            kind: ObjectKind::Table,
            columns: columns
                .iter()
                .map(|(c, key)| ColumnInfo {
                    name: (*c).into(),
                    type_name: "integer".into(),
                    primary_key: *key,
                    ..Default::default()
                })
                .collect(),
            foreign_keys: fks,
            ..Default::default()
        };
        let fk = |name: &str, col: &str, to: &str| ForeignKey {
            name: name.into(),
            columns: vec![col.into()],
            ref_namespace: Some("public".into()),
            ref_table: to.into(),
            ref_columns: vec!["id".into()],
            on_delete: None,
        };
        Schema {
            objects: vec![
                table(
                    "order_items",
                    &[("id", true), ("order_id", false)],
                    vec![fk("items_order", "order_id", "orders")],
                ),
                table(
                    "orders",
                    &[("id", true), ("customer_id", false)],
                    vec![fk("orders_customer", "customer_id", "customers")],
                ),
                table("customers", &[("id", true), ("name", false)], Vec::new()),
                table("settings", &[("key", true)], Vec::new()),
            ],
            truncated: false,
        }
    }

    #[test]
    fn referenced_tables_sit_left_of_their_references() {
        let d = diagram(&schema(), &Layout::default());
        let x = |name: &str| d.nodes.iter().find(|n| n.name == name).unwrap().x;
        assert!(x("customers") < x("orders") && x("orders") < x("order_items"));
        // Unrelated tables go after the related ones.
        assert!(x("settings") > x("order_items"));
        assert_eq!(d.edges.len(), 2);
        let orders = d.nodes.iter().find(|n| n.name == "orders").unwrap();
        assert!(orders.fields[1].reference && orders.fields[0].key);
        // Lines leave the referencing row and enter the referenced one.
        let edge = d
            .edges
            .iter()
            .find(|e| e.name == "orders_customer")
            .unwrap();
        let points = route(&d, edge);
        assert_eq!(points.first().unwrap().1, orders.row_y("customer_id"));
        assert_eq!(points.first().unwrap().0, orders.x);
    }

    #[test]
    fn saved_positions_win_and_round_trip() {
        let mut saved = Layout::default();
        saved
            .tables
            .insert("public.orders".into(), Position { x: 900., y: 700. });
        let d = diagram(&schema(), &saved);
        let orders = d.nodes.iter().find(|n| n.name == "orders").unwrap();
        assert_eq!((orders.x, orders.y), (900., 700.));
        let path = layout_path(&crate::testing::dir("erd-layout"), "DATABASE_URL (prod)");
        assert!(path.ends_with(".solder/erd/DATABASE_URL__prod_.json"));
        Layout::of(&d).save(&path).unwrap();
        assert_eq!(Layout::load(&path), Layout::of(&d));
        assert!(orders.contains(910., 710.) && !orders.contains(10., 10.));
        assert_eq!(
            orders.field_at(700. + HEADER + ROW + 3.).unwrap().name,
            "customer_id"
        );
    }

    #[test]
    fn exports() {
        let d = diagram(&schema(), &Layout::default());
        let m = mermaid(&d);
        assert!(m.starts_with(
            "erDiagram\n  order_items {\n    integer id PK\n    integer order_id FK\n  }"
        ));
        assert!(m.contains("  customers ||--o{ orders : \"customer_id\"\n"));
        let edge = d
            .edges
            .iter()
            .find(|e| e.name == "orders_customer")
            .unwrap();
        assert_eq!(
            join_query(Engine::Postgres, &d, edge),
            "SELECT * FROM \"orders\" c JOIN \"customers\" p ON c.\"customer_id\" = p.\"id\" LIMIT 200"
        );
        let svg = svg(&d, &Palette::default());
        assert!(
            svg.starts_with("<svg")
                && svg.contains(">customers</text>")
                && svg.contains("<polyline")
        );
        let png = png(&svg, 1.).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn routes_around_tables_in_between() {
        let mut d = diagram(&schema(), &Layout::default());
        // items -> customers skips orders, which sits in the middle column.
        let (items, orders, customers) = (0, 1, 2);
        d.edges.push(Edge {
            name: "items_customer".into(),
            from: items,
            from_columns: vec!["id".into()],
            to: customers,
            to_columns: vec!["id".into()],
        });
        // As in a real schema: orders across the source row, the target row
        // below orders. Bending midway would run through orders.
        let y = d.nodes[items].row_y("id");
        d.nodes[orders].y = y - 10.;
        d.nodes[customers].y = d.nodes[orders].y + d.nodes[orders].height() + 40.;
        let edge = d.edges.last().unwrap().clone();
        let (x0, x1) = (
            d.nodes[items].x,
            d.nodes[customers].x + d.nodes[customers].width,
        );
        let y1 = d.nodes[customers].row_y("id");
        let midway = [(x0, y), ((x0 + x1) / 2., y), ((x0 + x1) / 2., y1), (x1, y1)];
        assert_eq!(crossings(&d, &edge, &midway), 1);
        assert_eq!(crossings(&d, &edge, &route(&d, &edge)), 0);
    }
}
