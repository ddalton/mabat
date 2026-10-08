#!/usr/bin/env python3
"""Build `mabat-architecture.vsdx` — the figures of Mabat's architecture
document, one Visio page per figure.

The document explains how Mabat works: a view declares the shape of the
data; the derive turns it into a static shape, decoders and encoders;
the planner turns the shape into a tree of batched queries; the executor
runs them on PostgreSQL, MySQL or SQLite; a DBA can replace any query;
and the same shape writes the aggregate back. Every claim on these pages
is a rule of the MPA specification (docs/mpa.md), cited by its number.

The last figure, the contract, is READ FROM docs/mpa.json at build time:
every capability against the areas of the specification, with the rules
that define it. It cannot drift from the index, and the index cannot
drift from the specification (crates/mabat/tests/mpa.rs).

The kit (vsdxkit, dataflowkit, vsdxemf) is flint's poster kit, copied
into docs/architecture/.

Run:  python3 mabat-visio.py [outdir] [--svg] [--emf] [--pdf]

  --svg   write each page as diagrams/NN-name.svg for the HTML document
  --emf   write one EMF per page, for pasting into Office
  --pdf   render the pages alone to mabat-figures.pdf

A non-zero exit means a gate found something. Run the gates, read
"N shapes, M pages, 0 problems", and then LOOK at the render: the gates
are necessary, not sufficient.
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import dataflowkit as d      # noqa: E402

_here = os.path.dirname(os.path.abspath(__file__))
_repo = os.path.dirname(os.path.dirname(os.path.dirname(_here)))
k = d.k
emf = d.emf

INK, SUB, MUTE, PAPER = d.INK, d.SUB, d.MUTE, d.PAPER
FLOW, DUR, CTL, ALT = d.FLOW, d.DUR, d.CTL, d.ALT
W, WT = d.W, d.WT

# Mabat's own hue, the indigo of the documentation site; an ORM is drawn
# in a neutral slate, as the thing compared against.
MABAT = "#4338CA"
MABAT_F = "#E9E8FB"
ORM = "#5B6472"

RED_F, RED_L, RED_T = "#FDECEC", "#C0392B", "#8C2F22"
OK_F, OK_L, OK_T = "#E8F5EC", "#2F7D4A", "#1F5A34"
ROW_F, ROW_L = "#F6F7F9", "#DCE0E5"
CODE_F, CODE_L = "#F7F8FB", "#D5DBE3"

# Roles, the same in every figure:
APP_F, APP_L = d.CLIENT_F, d.CLIENT_L    # the application's code and values
GEN_F, GEN_L = d.WORK_F, d.WORK_L        # what the derive generates
PLAN_F, PLAN_L = d.DOOR_F, d.DOOR_L      # mabat-core: shapes and plans
RUN_F, RUN_L = d.SERV_F, d.SERV_L        # mabat-sqlx: running queries
DBA_F, DBA_L = d.OPER_F, d.OPER_L        # overrides, a DBA's work
DB_F, DB_L = d.S3_F, d.S3_L              # the database

LINE = 0.012


# ---------------------------------------------------------------------
# pieces
# ---------------------------------------------------------------------
def bar(p, x, y, w, text, color, h=0.40, size=10.5):
    return p.box(x, y, w, h, text, "", fill=color, line=color,
                 title_size=size, title_color=PAPER, halign=1, valign=1,
                 rounding=0.05)


def fit_h(title, body, w, title_size=9.4, body_size=7.8, pad=0.18):
    """The height a centred node needs for its text."""
    _, _, need = k.measure(title, body, w, title_size, body_size)
    return need + pad


def rect(p, x, y, w, h, title, body, f, l, **kw):
    kw.setdefault("title_size", 9.4)
    kw.setdefault("body_size", 7.8)
    kw.setdefault("line_weight", LINE)
    if h is None:
        h = fit_h(title, body, w, kw["title_size"], kw["body_size"])
    return d.node(p, "rect", x, y, w, h, title, body, fill=f, line=l, **kw)


def code(p, x, y, w, text, size=7.4, h=None):
    """Source code or SQL, in a quiet panel, never wrapped by the kit."""
    inner = w - 0.24
    for line in text.split("\n"):
        assert len(k.wrap(line, inner, size, mono=True)) <= 1, \
            "a code line wraps at %.2f\": %r" % (w, line)
    _, _, need = k.measure(text, "", inner, size, 7.5, False, True)
    bg = p.box(x, y, w, max(need + 0.18, h or 0), "", "", fill=CODE_F,
               line=CODE_L, line_weight=0.008, rounding=0.05)
    p.text(x + 0.12, y + 0.08, inner, text, size=size, color=INK, mono=True)
    return bg


def rows(p, xs, w, y, table, gap=0.08, key_size=7.4, body_size=8.0):
    """Attribute rows under a set of columns, levelled across columns."""
    for key, vals in table:
        boxes = [p.fitbox(x, y, w, key, v, pad=0.10, title_size=key_size,
                          body_size=body_size, title_color=MUTE,
                          body_color=INK, fill=ROW_F, line=ROW_L,
                          line_weight=0.008, rounding=0.04)
                 for x, v in zip(xs, vals)]
        tall = max(b.h for b in boxes)
        for b in boxes:
            b.h = tall
        y += tall + gap
    return y


def right(b, dy=None):
    return (b.x + b.w, b.y + (b.h / 2 if dy is None else dy))


def left(b, dy=None):
    return (b.x, b.y + (b.h / 2 if dy is None else dy))


def harrow(p, a, b, color=FLOW, weight=W, dy=None, **kw):
    """A horizontal arrow from box a's right edge to box b's left edge."""
    y = a.y + (min(a.h, b.h) / 2 if dy is None else dy)
    return p.arrow([(a.x + a.w, y), (b.x, y)], color=color, weight=weight, **kw)


def varrow(p, a, b, color=FLOW, weight=W, x=None, **kw):
    """A vertical arrow from box a's bottom edge to box b's top edge."""
    x = a.x + a.w / 2 if x is None else x
    return p.arrow([(x, a.y + a.h), (x, b.y)], color=color, weight=weight, **kw)


# =====================================================================
# 1 · Mabat at a glance
# =====================================================================
def fig_glance(doc):
    p = doc.page("1 At a glance", 17, 10)
    cw, gap = 2.90, 0.50
    xs = [0.25 + i * (cw + gap) for i in range(5)]
    heads = [("1 · you declare", "views: Rust structs", APP_L),
             ("2 · the derive generates", "mabat-derive, at compile time", GEN_L),
             ("3 · the planner plans", "mabat-core, no database", PLAN_L),
             ("4 · the executor runs", "mabat-sqlx, on SQLx 0.9", RUN_L),
             ("5 · the database", "an existing schema", DB_L)]
    for x, (h, s, c) in zip(xs, heads):
        bar(p, x, 0.3, cw, h, c, size=10)
        p.text(x, 0.76, cw, s, size=8.0, color=MUTE, halign=1)

    top = 1.15
    view = code(p, xs[0], top, cw,
                "#[derive(View)]\n"
                "#[view(table = \"task\")]\n"
                "struct TaskView {\n"
                "    id: Uuid,\n"
                "    name: String,\n"
                "    #[view(to_one(fk = \"assignee_id\"))]\n"
                "    assignee: Option<PersonView>,\n"
                "    #[view(child(fk = \"task_id\"))]\n"
                "    notes: Vec<NoteView>,\n"
                "}", size=7.0)
    h1 = view.h
    gen = rect(p, xs[1], top, cw, h1, "a static shape",
               "its columns, embedded values, references, collections and "
               "variants as data (mabat::shape)\n\n"
               "a decoder and an encoder for each enabled database\n\n"
               "no reflection at run time", GEN_F, GEN_L)
    plan = rect(p, xs[2], top, cw, h1, "a plan: a tree of queries",
                "one root query, and one query per reference, collection and "
                "variant table, each named by its path: $root, assignee, notes\n\n"
                "SQL in the database's dialect", PLAN_F, PLAN_L)
    run = rect(p, xs[3], top, cw, h1, "the executor",
               "runs the root query, then each level with the keys of the "
               "rows above it\n\n"
               "decodes every row by alias, and attaches children by "
               "$parent", RUN_F, RUN_L)
    db = d.node(p, "cylinder", xs[4], top, cw, 5.05,
                "PostgreSQL · MySQL 8 · SQLite",
                "through a connection, a transaction, or a Pooled pool",
                fill=DB_F, line=DB_L, line_weight=LINE, cap=0.30,
                title_size=9.6, body_size=7.8)
    harrow(p, view, gen, CTL)
    harrow(p, gen, plan, CTL)
    harrow(p, plan, run, CTL)
    p.arrow([(run.x + run.w, top + 0.55), (db.x, top + 0.55)], color=FLOW,
            weight=WT)
    d.flabel(p, run.x + run.w + gap / 2, top + 0.30, "SQL", w=0.45, size=7.2)
    p.arrow([(db.x, top + 1.05), (run.x + run.w, top + 1.05)], color=FLOW,
            weight=W)
    d.flabel(p, run.x + run.w + gap / 2, top + 1.30, "rows", w=0.45, size=7.2)

    # ---- overrides and results -------------------------------------------
    y2 = top + h1 + 0.75
    files = rect(p, xs[1], y2, cw, None, "override files",
                 "TaskView.toml or TaskView.sql: hand-written SQL for any "
                 "query, by a DBA", DBA_F, DBA_L)
    reg = rect(p, xs[2], y2, cw, files.h, "the registry",
               "checks every query against the view when it is built; "
               "reload swaps them in", DBA_F, DBA_L)
    out = rect(p, xs[3], y2, cw, files.h, "what a load returns",
               "Vec<T>, Option<T> or Graph<T>  ·  serde_json::Value  ·  "
               "a GraphQL response", APP_F, APP_L)
    harrow(p, files, reg, CTL)
    ry = top + h1 + 0.35
    p.arrow([(reg.x + reg.w - 0.5, reg.y), (reg.x + reg.w - 0.5, ry),
             (run.x + 0.5, ry), (run.x + 0.5, run.y + run.h)], color=CTL,
            weight=W)
    d.flabel(p, reg.x + reg.w + gap / 2 + 0.05, ry - 0.22,
             "replaces SQL", CTL, w=1.0, size=7.2)
    varrow(p, run, out, FLOW, x=run.x + 2.0)

    # ---- writes -----------------------------------------------------------
    y3 = y2 + files.h + 0.75
    p.text(xs[0], y3 - 0.36, 9.0, "Writing: the same shape, the other way",
           size=9.4, color=DUR, bold=True)
    call = code(p, xs[0], y3, cw,
                "mabat::save(&mut task, &mut tx)\n"
                "mabat::save_changes(\n"
                "    &before, &mut after, &mut tx)\n"
                "mabat::delete::<TaskView, _>(id, tx)", size=7.0)
    hw = call.h
    enc = rect(p, xs[1], y3, cw, hw, "the encoder",
               "a tree of rows: the view's row, its owned collections, "
               "links and variant rows", GEN_F, GEN_L)
    stm = rect(p, xs[2], y3, cw, hw, "write statements",
               "upserts by key, updates of what changed, deletes deepest "
               "first", PLAN_F, PLAN_L)
    tx = rect(p, xs[3], y3, cw, hw, "one transaction",
              "or a savepoint of yours; statements run when called, with no "
              "session and no flush", RUN_F, RUN_L)
    harrow(p, call, enc, DUR)
    harrow(p, enc, stm, DUR)
    harrow(p, stm, tx, DUR)
    p.arrow([(tx.x + tx.w, tx.y + hw / 2), (db.x, tx.y + hw / 2)],
            color=DUR, weight=WT)
    db.h = tx.y + hw + 0.10 - db.y

    # ---- against an ORM ----------------------------------------------------
    y4 = y3 + hw + 0.45
    half = (16.5 - 0.3) / 2
    cx = [0.25, 0.25 + half + 0.3]
    bar(p, cx[0], y4, half, "an ORM  ·  JPA and Hibernate, Diesel "
        "associations, SeaORM", ORM, size=9.6)
    bar(p, cx[1], y4, half, "Mabat", MABAT, size=9.6)
    rows(p, cx, half, y4 + 0.50, [
        ("THE UNIT OF A READ", [
            "an entity; what else loads depends on what the code touches "
            "(lazy loading) or on fetch joins chosen in code",
            "a declared view, loaded whole: the roots, and everything the "
            "view declares (MPA-CORE-2, MPA-NOT-4)"]),
        ("THE QUERIES", [
            "decided at run time by navigation: N+1 queries, or joins that "
            "multiply collections, unless tuned in code",
            "fixed by the shape and printable before running: one per "
            "relationship, never one per row, never a join of two "
            "collections (MPA-PLAN-1)"]),
        ("WHO OWNS THE SQL", [
            "the ORM; changing a query means changing and releasing code",
            "generated, or replaced per query by a DBA and checked against "
            "the type at startup (MPA-OVR-5)"]),
        ("STATE AND WRITES", [
            "a session: identity map, snapshots, dirty checking and a flush",
            "none: values are plain structs; save, save_changes and delete "
            "run when called (MPA-WRITE-1)"]),
    ])
    return p


# =====================================================================
# 2 · a view is a shape
# =====================================================================
def fig_shape(doc):
    p = doc.page("2 A view is a shape", 17, 9)
    bar(p, 0.25, 0.3, 6.6, "the declaration", APP_L, size=10)
    src = code(p, 0.25, 0.85, 6.6,
               "#[derive(View)]\n"
               "#[view(table = \"task\")]\n"
               "struct TaskView {\n"
               "    id: Uuid,                     // the key\n"
               "    name: String,\n"
               "    #[view(json)]\n"
               "    labels: Vec<String>,\n"
               "    #[view(embed(prefix = \"due_\"))]\n"
               "    due: Window,                 // due_start, due_end\n"
               "    #[view(embed)]\n"
               "    state: State,                // an enum with data\n"
               "    #[view(to_one(fk = \"assignee_id\"))]\n"
               "    assignee: Option<PersonView>,\n"
               "    #[view(child(fk = \"task_id\", order_by = \"created_at\"))]\n"
               "    notes: Vec<NoteView>,\n"
               "    #[view(child(fk = \"parent_id\", depth = 2))]\n"
               "    children: Vec<TaskView>,\n"
               "}\n"
               "\n"
               "#[derive(View)]\n"
               "#[view(embedded)]\n"
               "struct Window { start: NaiveDate, end: Option<NaiveDate> }\n"
               "\n"
               "#[derive(View)]\n"
               "#[view(tag = \"state\")]\n"
               "enum State { Open, Blocked { reason: String }, Done }",
               size=8.6)

    x0 = 7.35
    bar(p, x0, 0.3, 9.4, "the shape the derive generates, and how it loads",
        GEN_L, size=10)
    # the row: everything decoded from one row of task
    zy, bh = 0.95, 1.30
    zh = 0.85 + bh + 0.3
    d.zone(p, x0, zy, 9.4, zh, "one row of task, in the $root query",
           sub="decoded by alias; each path is the alias of its column "
               "(MPA-CORE-4, MPA-PLAN-2)")
    cw = 2.15
    gx = [x0 + 0.25 + i * (cw + 0.17) for i in range(4)]
    cy = zy + 0.85
    cols = rect(p, gx[0], cy, cw, bh, "columns",
                "id  ·  name\n\nlabels: a JSON column, read with serde "
                "(MPA-VIEW-6)", APP_F, APP_L)
    emb = rect(p, gx[1], cy, cw, bh, "an embedded struct",
               "due.start  ·  due.end\n\nin columns due_start and due_end "
               "of the same row (MPA-VIEW-7)", APP_F, APP_L)
    enum = rect(p, gx[2], cy, cw, bh, "an enum with data",
                "state.$tag names the variant\n\nstate.Blocked.reason is a "
                "column of the row; decoding is strict (MPA-SUM-4)",
                APP_F, APP_L)
    ref = rect(p, gx[3], cy, cw, bh, "a reference's key",
               "$ref.assignee: the foreign key assignee_id, whose rows the "
               "assignee query fetches", APP_F, APP_L)

    # the queries below
    qy = zy + zh + 0.75
    qw = 2.95
    qx = [x0 + i * (qw + 0.275) for i in range(3)]
    qa = rect(p, qx[0], qy, qw, None, "assignee  ·  a to-one reference",
              "PersonView rows WHERE id IN the $ref.assignee keys; a NULL "
              "key is None (MPA-VIEW-8)", PLAN_F, PLAN_L)
    qn = rect(p, qx[1], qy, qw, qa.h, "notes  ·  a collection",
              "NoteView rows WHERE task_id IN the tasks' keys, ordered by "
              "created_at, then the key (MPA-VIEW-9)", PLAN_F, PLAN_L)
    qc = rect(p, qx[2], qy, qw, qa.h, "children  ·  recursive, depth 2",
              "TaskView again, one query per level: children, "
              "children.children (MPA-PLAN-4)", PLAN_F, PLAN_L)
    for q in (qa, qn, qc):
        cxq = q.x + q.w / 2
        p.arrow([(cxq, zy + zh), (cxq, q.y)], color=ALT, weight=W)
    d.flabel(p, qa.x + qa.w / 2 + 0.75, zy + zh + 0.30, "the keys of the rows",
             ALT, w=1.35, size=7.2)

    # what the derive generates
    gy = qy + qa.h + 0.75
    p.text(x0, gy - 0.34, 9.4, "what the derive generates for each view, "
           "at compile time (MPA-CORE-3)", size=9.0, color=GEN_L, bold=True)
    gw = (9.4 - 3 * 0.17) / 4
    for i, (t, b) in enumerate([
            ("mabat::shape", "the static description the planner reads"),
            ("a decoder per database", "rows to values, by alias"),
            ("an encoder per database", "values to rows, for save"),
            ("a comparison", "what changed, for save_changes")]):
        rect(p, x0 + i * (gw + 0.17), gy, gw, None, t, b, GEN_F, GEN_L,
             title_size=8.8, body_size=7.6)
    return p


# =====================================================================
# 3 · from shape to queries
# =====================================================================
SQL_ROOT = ('SELECT t0."id" AS "id", t0."name" AS "name",\n'
            '       t0."assignee_id" AS "$ref.assignee"\n'
            'FROM "task" AS t0\n'
            'WHERE t0."name" ILIKE $1\n'
            'ORDER BY t0."name" LIMIT 20')
SQL_ASSIGNEE = ('SELECT t0."id" AS "id", t0."name" AS "name"\n'
                'FROM "person" AS t0\n'
                'WHERE t0."id" = ANY($1)')
SQL_NOTES = ('SELECT t0."id" AS "id", t0."task_id" AS "$parent",\n'
             '       t0."body" AS "body"\n'
             'FROM "note" AS t0\n'
             'WHERE t0."task_id" = ANY($1)\n'
             'ORDER BY t0."created_at", t0."id"')
SQL_SUBTASKS = ('SELECT t0."id" AS "id", t0."parent_id" AS "$parent",\n'
                '       t0."name" AS "name",\n'
                '       t0."assignee_id" AS "$ref.assignee"\n'
                'FROM "task" AS t0\n'
                'WHERE t0."parent_id" = ANY($1)\n'
                'ORDER BY t0."id"')


def query(p, x, y, w, name, link, sql, h=None):
    """A query of the plan: its name, how it is linked, and its SQL."""
    head = p.box(x, y, w, 0.34, name + "   " + link, "", fill=PLAN_L,
                 line=PLAN_L, title_size=8.6, title_color=PAPER, valign=1,
                 rounding=0.04, title_mono=False)
    body = code(p, x, y + 0.34, w, sql, size=7.0, h=h)
    return head, body


def fig_plan(doc):
    p = doc.page("3 From shape to queries", 17, 9)
    bar(p, 0.25, 0.3, 11.6, "the plan of TaskView, as mabat::plan and explain "
        "print it  ·  PostgreSQL", PLAN_L, size=10)
    w1 = 5.6
    x1, x2 = 0.25, 0.25 + w1 + 0.4
    rh, rb = query(p, x1, 0.95, w1, "$root", "the root", SQL_ROOT)
    y = rb.y + rb.h + 0.55
    ah, ab = query(p, x1, y, w1, "assignee", "to-one by $ref.assignee",
                   SQL_ASSIGNEE)
    nh, nb = query(p, x2, 0.95, w1, "notes", "to-many by task_id", SQL_NOTES,
                   h=rb.h)
    sh, sb = query(p, x2, y, w1, "subtasks", "to-many by parent_id",
                   SQL_SUBTASKS)
    y2 = max(ab.y + ab.h, sb.y + sb.h) + 0.55
    th, tb = query(p, x2, y2, w1, "subtasks.assignee",
                   "to-one by $ref.assignee", SQL_ASSIGNEE)

    # keys flow down the tree
    p.arrow([(x1 + 1.0, rb.y + rb.h), (x1 + 1.0, ah.y)], color=ALT, weight=W)
    d.flabel(p, x1 + 2.55, rb.y + rb.h + 0.27,
             "the distinct $ref.assignee keys", ALT, w=2.6, size=7.2)
    p.arrow([(rb.x + rb.w, rb.y + 0.5), (nb.x, rb.y + 0.5)], color=ALT,
            weight=W)
    sy = sh.y + 0.17
    p.arrow([(rb.x + rb.w, rb.y + rb.h - 0.3), (rb.x + rb.w + 0.2,
             rb.y + rb.h - 0.3), (rb.x + rb.w + 0.2, sy), (sh.x, sy)],
            color=ALT, weight=W)
    d.flabel(p, rb.x + rb.w - 0.75, rb.y + rb.h + 0.27, "the tasks' keys",
             ALT, w=1.2, size=7.2)
    p.arrow([(sb.x + 1.0, sb.y + sb.h), (sb.x + 1.0, th.y)], color=ALT,
            weight=W)
    d.flabel(p, sb.x + 2.85, sb.y + sb.h + 0.27,
             "the $ref.assignee keys of the subtasks", ALT, w=3.0, size=7.2)

    fs = p.stack(x1, ab.y + ab.h + 0.45, w1, gap=0.10)
    for t, b in [
            ("keys flow down, rows flow up", "each query takes the keys of "
             "the rows above it; its rows are grouped by $parent and "
             "attached to their parents"),
            ("one array, or a padded list", "= ANY($1) on PostgreSQL; IN "
             "(?, …) on MySQL and SQLite, padded to a power of two and at "
             "most 1,000 keys a statement (MPA-DB-4)"),
            ("by alias, never by position", "a query may select its columns "
             "in any order, and extra columns are ignored (MPA-PLAN-3)")]:
        fs.box(t, b, fill=ROW_F, line=ROW_L, line_weight=0.008,
               title_size=8.6, body_size=7.6, title_color=INK, pad=0.12)

    # the view and the counts, at the right
    x3 = 12.25
    bar(p, x3, 0.3, 4.5, "the view", APP_L, size=10)
    v = code(p, x3, 0.95, 4.5,
             "struct TaskView {\n"
             "    id: Uuid,\n"
             "    name: String,\n"
             "    assignee: Option<PersonView>,\n"
             "    notes: Vec<NoteView>,\n"
             "    subtasks: Vec<SubtaskView>,\n"
             "}\n"
             "struct SubtaskView {\n"
             "    id: Uuid,\n"
             "    name: String,\n"
             "    assignee: Option<PersonView>,\n"
             "}", size=7.0)
    st = p.stack(x3, v.y + v.h + 0.3, 4.5, gap=0.12)
    st.box("20 tasks, 4 subtasks each", "", fill=PAPER, no_line=True,
           title_size=9.4, pad=0.04)
    for t, b, f, l in [
            ("5 queries with Mabat", "one per query of the plan, whatever "
             "the number of rows (MPA-PLAN-1)", OK_F, OK_L),
            ("181 queries, one per row", "1 for the tasks, then 20 for "
             "assignees, 20 for notes, 20 for subtasks and 80 for their "
             "assignees: the N+1 pattern of lazy loading", RED_F, RED_L),
            ("1 query with joins", "but notes × subtasks rows per task: "
             "every note repeated for every subtask", RED_F, RED_L)]:
        st.box(t, b, fill=f, line=l, title_size=9.0, body_size=7.6,
               line_weight=LINE, pad=0.14)
    return p


# =====================================================================
# 4 · overrides
# =====================================================================
def fig_overrides(doc):
    p = doc.page("4 Overrides", 17, 9)
    cw = 5.2
    xs = [0.25, 0.25 + cw + 0.45, 0.25 + 2 * (cw + 0.45)]
    bar(p, xs[0], 0.3, cw, "1 · a DBA writes SQL", DBA_L, size=10)
    bar(p, xs[1], 0.3, cw, "2 · build checks every query", PLAN_L, size=10)
    bar(p, xs[2], 0.3, cw, "3 · loads run the registry", RUN_L, size=10)

    f = code(p, xs[0], 0.95, cw,
             "# overrides/TaskView.toml\n"
             "[query.\"notes\"]\n"
             "sql = \"\"\"\n"
             "SELECT n.id, n.task_id AS \"$parent\", n.body\n"
             "FROM note n\n"
             "JOIN note_visible v ON v.note_id = n.id\n"
             "WHERE n.task_id = ANY(:keys)\n"
             "ORDER BY n.created_at, n.id\n"
             "\"\"\"\n"
             "shadow = true", size=7.2)
    st = p.stack(xs[0], f.y + f.h + 0.25, cw, gap=0.10)
    for t, b in [
            ("one file per view", "TaskView.toml, or TaskView.sql with a "
             "-- mabat: query notes line before each query (MPA-OVR-2)"),
            ("any query, by its path", "$root, assignee, notes, "
             "children.notes: the names of the plan (MPA-CORE-5)"),
            ("the aliases the view decodes", "its columns, the key, $parent "
             "for a collection, $ref.<field> for a reference (MPA-OVR-3)"),
            ("the keys of the rows above", ":keys, or $1 on PostgreSQL "
             "(MPA-OVR-4)")]:
        st.box(t, b, fill=ROW_F, line=ROW_L, line_weight=0.008,
               title_size=8.6, body_size=7.6, title_color=INK, pad=0.12)

    # build: prepare and compare
    b1 = rect(p, xs[1], 0.95, cw, None,
              "Mabat::builder().register::<TaskView>()…build(&mut conn)",
              "for every query of every view, generated or overridden",
              PLAN_F, PLAN_L, title_size=8.6)
    b2 = rect(p, xs[1], b1.y + b1.h + 0.45, cw, None, "prepare, without running",
              "the database parses the SQL and describes its columns and "
              "parameters", PLAN_F, PLAN_L)
    b3 = rect(p, xs[1], b2.y + b2.h + 0.45, cw, None, "compare with the view",
              "every alias the view decodes, with a compatible type; the keys "
              "as the only parameter", PLAN_F, PLAN_L)
    varrow(p, b1, b2, CTL)
    varrow(p, b2, b3, CTL)
    p.text(xs[1], b3.y + b3.h + 0.3, cw, "diagnostics, each with the view, "
           "the query, the file and the line", size=8.4, color=INK, bold=True)
    dg = code(p, xs[1], b3.y + b3.h + 0.62, cw,
              "M0100  the file cannot be read or parsed\n"
              "M0101  no such view, or no such query\n"
              "M0102  missing, extra or wrongly typed aliases\n"
              "M0103  the query does not prepare\n"
              "M0104  the wrong parameters\n"
              "M0105  warning: an optional path is never selected\n"
              "M0301  the view cannot be planned", size=7.6)
    after = rect(p, xs[1], dg.y + dg.h + 0.3, cw, None, "errors stop the build",
                 "Error::Invalid with the report, or on_invalid(UseGenerated) "
                 "runs the generated query instead (MPA-OVR-5)",
                 RED_F, RED_L, body_color=RED_T)
    p.arrow([(f.x + f.w, f.y + 0.6), (b1.x, f.y + 0.6)], color=CTL, weight=W)

    # the registry
    r1 = rect(p, xs[2], 0.95, cw, None, "the registry",
              "immutable; Mabat::load::<TaskView>() runs the override where "
              "there is one, the generated query elsewhere (MPA-OVR-1)",
              DBA_F, DBA_L)
    p.arrow([(b1.x + b1.w, r1.y + 0.45), (r1.x, r1.y + 0.45)], color=CTL,
            weight=W)
    st3 = p.stack(xs[2], r1.y + r1.h + 0.35, cw, gap=0.18)
    for t, b, fc, lc in [
            ("shadow = true", "runs the override and the generated query, "
             "and logs a warning when their rows differ; shadow_stats counts "
             "runs and mismatches (MPA-OVR-6)", DBA_F, DBA_L),
            ("reload(&mut conn)", "reads the files again, checks them, and "
             "swaps them in atomically; invalid files leave the running "
             "overrides as they are (MPA-OVR-7)", DBA_F, DBA_L),
            ("nested arguments and paging", "wrap the override as a subquery "
             "and refer to columns by their aliases (MPA-LOAD-10)",
             RUN_F, RUN_L),
            ("writes never use overrides", "save and delete always run "
             "generated statements (MPA-NOT-5)", ROW_F, ORM)]:
        st3.box(t, b, fill=fc, line=lc, line_weight=LINE, title_size=9.0,
                body_size=7.6, pad=0.14)

    # the CLI, along the bottom
    yb = max(after.y + after.h, st3.bottom) + 0.45
    d.zone(p, 0.25, yb, 16.5, 1.35, "without a Rust toolchain: the mabat "
           "command line tool (MPA-OVR-8)")
    sw = 3.7
    sx = [0.5 + i * (sw + 0.35) for i in range(4)]
    for x, (t, b) in zip(sx, [
            ("Builder::manifest()", "the application writes its views as "
             "JSON"),
            ("mabat check", "the override files against the manifest and a "
             "database; --schema makes a scratch schema"),
            ("mabat explain", "every query of a view, generated or "
             "overridden"),
            ("mabat scaffold", "an override file from the generated SQL, "
             "to edit")]):
        rect(p, x, yb + 0.45, sw, 0.72, t, b, DBA_F if x != sx[0] else APP_F,
             DBA_L if x != sx[0] else APP_L, title_size=8.8, body_size=7.4)
    for i in range(3):
        p.arrow([(sx[i] + sw, yb + 0.81), (sx[i + 1], yb + 0.81)], color=CTL,
                weight=W)
    return p


# =====================================================================
# 5 · the shapes of a result
# =====================================================================
def mini(p, x, y, w, text, f, l, h=0.42, size=8.0):
    return p.box(x, y, w, h, text, "", fill=f, line=l, title_size=size,
                 halign=1, valign=1, rounding=0.05, line_weight=LINE)


def fig_results(doc):
    p = doc.page("5 Shapes of a result", 17, 8)
    cw = 3.9
    xs = [0.25 + i * (cw + 0.3) for i in range(4)]
    heads = [("a tree", "Vec<T>, Option<T>: the default"),
             ("shared values", "Arc<T>: decoded once per load"),
             ("a graph", "Ref<T>, loaded with graph()"),
             ("recursion", "depth = n, or recursive = \"cte\"")]
    for x, (h, s) in zip(xs, heads):
        bar(p, x, 0.3, cw, h, MABAT, size=10)
        p.text(x, 0.76, cw, s, size=8.0, color=MUTE, halign=1)
    top, zh = 1.15, 3.05
    for x in xs:
        d.zone(p, x, top, cw, zh, "")

    # a tree: copies
    x = xs[0]
    t1 = mini(p, x + 0.25, top + 0.35, 1.55, "task 1", APP_F, APP_L)
    t2 = mini(p, x + 2.1, top + 0.35, 1.55, "task 2", APP_F, APP_L)
    a1 = mini(p, x + 0.25, top + 1.45, 1.55, "Ada", APP_F, APP_L)
    a2 = mini(p, x + 2.1, top + 1.45, 1.55, "Ada", APP_F, APP_L)
    varrow(p, t1, a1, FLOW)
    varrow(p, t2, a2, FLOW)
    p.text(x + 0.25, top + 2.15, cw - 0.5,
           "each task owns its own copy of the person; the value is plain "
           "data, cloned and compared like any struct", size=7.8, color=SUB,
           halign=1)

    # Arc: one instance
    x = xs[1]
    t1 = mini(p, x + 0.25, top + 0.35, 1.55, "task 1", APP_F, APP_L)
    t2 = mini(p, x + 2.1, top + 0.35, 1.55, "task 2", APP_F, APP_L)
    a = mini(p, x + 1.175, top + 1.45, 1.55, "Arc: Ada", GEN_F, GEN_L)
    p.arrow([(t1.x + t1.w / 2, t1.y + t1.h), (t1.x + t1.w / 2, top + 1.2),
             (a.x + 0.45, top + 1.2), (a.x + 0.45, a.y)], color=FLOW, weight=W)
    p.arrow([(t2.x + t2.w / 2, t2.y + t2.h), (t2.x + t2.w / 2, top + 1.2),
             (a.x + a.w - 0.45, top + 1.2), (a.x + a.w - 0.45, a.y)],
            color=FLOW, weight=W)
    p.text(x + 0.25, top + 2.15, cw - 0.5,
           "decoded once per entity of the load and shared by everything "
           "that references it (MPA-LOAD-13)", size=7.8, color=SUB, halign=1)

    # graph: arena with cycles
    x = xs[2]
    ar = p.box(x + 0.25, top + 0.3, cw - 0.5, 1.75, "", "", fill=PAPER,
               line="#B9C0C8", dashed=True, rounding=0.1, line_weight=0.009)
    p.text(ar.x + 0.1, ar.y + 0.06, 2.6, "Graph<T>: each entity once",
           size=7.8, color="#5A646F", bold=True)
    e1 = mini(p, x + 0.45, top + 0.75, 1.4, "Ada", GEN_F, GEN_L)
    e2 = mini(p, x + 2.05, top + 0.75, 1.4, "Grace", GEN_F, GEN_L)
    p.arrow([(e1.x + e1.w, e1.y + 0.12), (e2.x, e1.y + 0.12)], color=FLOW,
            weight=W)
    p.arrow([(e2.x, e1.y + 0.30), (e1.x + e1.w, e1.y + 0.30)], color=FLOW,
            weight=W)
    p.text(x + 0.45, top + 1.35, cw - 0.9,
           "manager and reports: a cycle that ends by itself", size=7.4,
           color=SUB, halign=1)
    p.text(x + 0.25, top + 2.15, cw - 0.5,
           "typed references and generated navigation, "
           "task.manager(&graph); a view with Ref<T> must be loaded with "
           "graph (MPA-LOAD-14)", size=7.8, color=SUB, halign=1)

    # recursion
    x = xs[3]
    r1 = rect(p, x + 0.25, top + 0.3, 1.6, 1.75, "depth = 2",
              "one query per level:\nchildren,\nchildren.children\n\n"
              "stops after n levels", PLAN_F, PLAN_L, title_size=8.8,
              body_size=7.4)
    r2 = rect(p, x + 2.05, top + 0.3, 1.6, 1.75, "recursive = \"cte\"",
              "every level from one WITH RECURSIVE query; a path guard "
              "stops at cycles", PLAN_F, PLAN_L, title_size=8.8,
              body_size=7.4)
    p.text(x + 0.25, top + 2.15, cw - 0.5,
           "a cycle in the data of a cte collection fails with Error::Cycle "
           "(MPA-PLAN-4, MPA-LOAD-12)", size=7.8, color=SUB, halign=1)

    rows(p, xs, cw, top + zh + 0.25, [
        ("THE FIELD", ["T, Option<T>, Vec<T>, BTreeMap<K, T>",
                       "Arc<T>, Option<Arc<T>>, Vec<Arc<T>>",
                       "Ref<T>, Option<Ref<T>>, Vec<Ref<T>>",
                       "Vec<T> of the view itself"]),
        ("THE TERMINAL", ["all, one, optional, json", "all, one, optional, json",
                          "graph, or json with a selection (MPA-JSON-5)",
                          "all, one, optional, json"]),
        ("SAVING", ["save, save_changes, delete",
                    "save writes the referencing key only (MPA-WRITE-5)",
                    "not supported in 0.1 (MPA-NOT-2)",
                    "save writes every level it holds"]),
    ])
    return p


# =====================================================================
# 6 · concurrency
# =====================================================================
def lane_query(p, x, y, w, name, f=PLAN_F, l=PLAN_L, h=0.36):
    return p.box(x, y, w, h, name, "", fill=f, line=l, title_size=7.6,
                 halign=1, valign=1, rounding=0.04, line_weight=LINE)


def fig_concurrency(doc):
    p = doc.page("6 Concurrency", 17, 8.2)
    lw = 1.55              # lane labels
    # ---- one connection -------------------------------------------------
    bar(p, 0.25, 0.3, 16.5, "one connection or transaction: every query in "
        "turn, all seeing the transaction's own writes (MPA-DB-5)", RUN_L,
        size=10)
    y = 1.0
    p.text(0.25, y + 0.07, lw, "conn", size=8.4, color=INK, bold=True)
    x = 0.25 + lw
    for name, w in [("$root", 1.5), ("assignee", 1.7), ("notes", 1.7),
                    ("subtasks", 1.7), ("subtasks.assignee", 2.4)]:
        lane_query(p, x, y, w, name)
        x += w + 0.08
    p.text(x + 0.2, y + 0.04, 16.75 - x - 0.2,
           "five queries, one after another", size=8.0, color=MUTE)

    # ---- Pooled::snapshot ------------------------------------------------
    y0 = 2.0
    bar(p, 0.25, y0, 16.5, "Pooled::snapshot(&pool, 3): each level's queries at "
        "the same time, on one snapshot  ·  PostgreSQL (MPA-LOAD-11)",
        MABAT, size=10)
    ty = y0 + 0.65
    # level bands
    bands = [("start", 0.25 + lw, 3.5), ("level 0", 0.25 + lw + 3.6, 1.9),
             ("level 1", 0.25 + lw + 5.6, 2.3), ("level 2", 0.25 + lw + 8.0, 2.6),
             ("end", 0.25 + lw + 10.7, 2.1)]
    for name, bx, bw in bands:
        p.text(bx, ty, bw, name, size=8.0, color=MUTE, bold=True, halign=1)
    ly = ty + 0.35
    names = ["conn 1", "conn 2", "conn 3"]
    for i, n in enumerate(names):
        yy = ly + i * 0.52
        p.text(0.25, yy + 0.07, lw, n, size=8.4, color=INK, bold=True)
        sx, sw = bands[0][1], bands[0][2]
        if i == 0:
            lane_query(p, sx, yy, sw, "BEGIN REPEATABLE READ READ ONLY; "
                       "pg_export_snapshot()", RUN_F, RUN_L)
        else:
            lane_query(p, sx, yy, sw, "BEGIN …; SET TRANSACTION SNAPSHOT",
                       RUN_F, RUN_L)
        ex, ew = bands[4][1], bands[4][2]
        lane_query(p, ex, yy, ew, "ROLLBACK", RUN_F, RUN_L)
    lane_query(p, bands[1][1], ly, bands[1][2], "$root")
    for i, n in enumerate(["assignee", "notes", "subtasks"]):
        lane_query(p, bands[2][1], ly + i * 0.52, bands[2][2], n)
    lane_query(p, bands[3][1], ly, bands[3][2], "subtasks.assignee")
    nx = 0.25 + lw + 13.0
    p.text(nx, ly, 16.75 - nx, "every connection joins the snapshot before "
           "the first query, so a failing query cannot leave the others "
           "without one", size=7.8, color=SUB)

    ry = ly + 3 * 0.52 + 0.35
    for x, t, c in [(0.25, "a connection", RUN_L),
                    (5.85, "Pooled::snapshot", MABAT),
                    (11.45, "Pooled::read_committed", MABAT)]:
        bar(p, x, ry, 5.3, t, c, h=0.34, size=9.0)
    rows(p, [0.25, 5.85, 11.45], 5.3, ry + 0.44, [
        ("THE CONNECTION GIVEN", [
            "a connection, a transaction or a pooled connection: every "
            "query on it, in turn",
            "Pooled::snapshot(&pool, n): PostgreSQL only — asking for it "
            "on MySQL or SQLite does not compile",
            "Pooled::read_committed(&pool, n): any database; each query "
            "sees what is committed when it runs"]),
        ("CONNECTIONS", [
            "one, the caller's",
            "at most n, each held for one query; a load cannot deadlock "
            "on its own pool",
            "at most n, each held for one query"]),
        ("WHAT THE LOAD SEES", [
            "the transaction, including its uncommitted writes",
            "one consistent snapshot for every query of the load",
            "possibly a newer commit in a later level than in an earlier "
            "one"]),
        ("GRAPHS", [
            "in turn", "in turn on one connection: which query reaches an "
            "entity first decides where its row comes from",
            "in turn on one connection"]),
    ])
    return p


# =====================================================================
# 7 · JSON and GraphQL
# =====================================================================
def fig_serving(doc):
    p = doc.page("7 JSON and GraphQL", 17, 9)
    cw = 3.85
    xs = [0.25 + i * (cw + 0.367) for i in range(4)]
    heads = [("a request", APP_L), ("a selection", PLAN_L),
             ("the selected plan", PLAN_L), ("JSON", RUN_L)]
    for x, (h, c) in zip(xs, heads):
        bar(p, x, 0.3, cw, h, c, size=10)
    q = code(p, xs[0], 0.95, cw,
             "query {\n"
             "  tasks(where: { name: { ilike: \"%release%\" } },\n"
             "        limit: 20) {\n"
             "    name\n"
             "    assignee { name }\n"
             "    notes(orderBy: [{ createdAt: DESC }],\n"
             "          limit: 3) {\n"
             "      body\n"
             "    }\n"
             "  }\n"
             "}", size=6.8)
    s = rect(p, xs[1], 0.95, cw, q.h, "the selection set, as a Selection",
             "fields: name, assignee { name }, notes { body }\n\n"
             "arguments: the root's where and limit; nested arguments for "
             "notes, by its path (MPA-GQL-4, MPA-LOAD-9)\n\n"
             "fragments resolved, variables applied", PLAN_F, PLAN_L)
    pl = rect(p, xs[2], 0.95, cw, q.h, "build_selected",
              "only the selected columns, and only the selected child "
              "queries: three queries, not five\n\n"
              "recursion unrolled to the depth the selection asks for "
              "(MPA-JSON-3, MPA-JSON-5)", PLAN_F, PLAN_L)
    js = code(p, xs[3], 0.95, cw,
              "[{\n"
              "  \"name\": \"Release 1.2\",\n"
              "  \"assignee\": { \"name\": \"Ada\" },\n"
              "  \"notes\": [\n"
              "    { \"body\": \"Tagged\" },\n"
              "    { \"body\": \"Notes drafted\" },\n"
              "    { \"body\": \"Branch cut\" }\n"
              "  ]\n"
              "}]", size=6.8, h=q.h)
    harrow(p, q, s, CTL, dy=0.5)
    harrow(p, s, pl, CTL, dy=0.5)
    harrow(p, pl, js, FLOW, dy=0.5)

    # per-parent paging
    y = q.y + q.h + 0.55
    p.text(0.25, y, 10, "notes(limit: 3) is per task, in the notes query: "
           "ROW_NUMBER() OVER (PARTITION BY the parent)", size=9.4,
           color=PLAN_L, bold=True)
    code(p, 0.25, y + 0.35, 8.6,
         'SELECT * FROM (\n'
         '  SELECT t0."id" AS "id", t0."task_id" AS "$parent",\n'
         '         t0."body" AS "body",\n'
         '         ROW_NUMBER() OVER (PARTITION BY t0."task_id"\n'
         '           ORDER BY t0."created_at" DESC, t0."id") AS "$row"\n'
         '  FROM "note" AS t0\n'
         '  WHERE t0."task_id" = ANY($1)\n'
         ') AS p WHERE p."$row" <= 3 ORDER BY p."$row"', size=7.2)
    x2 = 9.2
    st = p.stack(x2, y + 0.35, 16.75 - x2, gap=0.12)
    for t, b, f, l in [
            ("the schema is generated from the views",
             "mabat_graphql::schema(&pool).list::<TaskView>(\"tasks\")"
             ".by_key::<TaskView>(\"task\"): objects for views, unions for "
             "enums with data, filters from the column types (MPA-GQL-1, "
             "MPA-GQL-2)", GEN_F, GEN_L),
            ("one load per root field",
             "the whole selection set, every level batched: no resolver "
             "per field, so no N+1 (MPA-GQL-4)", OK_F, OK_L),
            ("the same, without GraphQL",
             "load::<TaskView>().select(Selection::parse(\"name assignee "
             "{ name }\")).json(conn): for a REST endpoint (MPA-JSON-3)",
             APP_F, APP_L),
            ("overrides apply", "a registry's overrides serve selections "
             "and GraphQL too (MPA-JSON-6)", DBA_F, DBA_L)]:
        st.box(t, b, fill=f, line=l, line_weight=LINE, title_size=9.0,
               body_size=7.6, pad=0.14)
    return p


# =====================================================================
# 8 · writes
# =====================================================================
def fig_writes(doc):
    p = doc.page("8 Writes", 17, 9.4)
    cw = 5.2
    xs = [0.25, 0.25 + cw + 0.45, 0.25 + 2 * (cw + 0.45)]
    bar(p, xs[0], 0.3, cw, "mabat::save(&mut board, &mut tx)", APP_L, size=10)
    bar(p, xs[1], 0.3, cw, "the statements, in one transaction", PLAN_L,
        size=10)
    bar(p, xs[2], 0.3, cw, "with #[view(version)]", MABAT, size=10)

    # the tree of rows
    x = xs[0]
    root = rect(p, x, 0.95, cw, None, "Board  ·  the board row",
                "name, owner_id (the reference's key only), the state tag "
                "and its variant's columns, color_red …", GEN_F, GEN_L,
                title_size=9.0, body_size=7.6)
    kids = [("lists  ·  Vec, index = \"position\"",
             "list rows with board_id and their position from 0; each list "
             "owns its cards"),
            ("labels  ·  many-to-many",
             "board_label link rows only; labels are not written"),
            ("settings  ·  BTreeMap by name",
             "setting rows with board_id and their map key"),
            ("visibility  ·  a table per variant",
             "the public_board row, or none")]
    y = root.y + root.h + 0.3
    for t, b in kids:
        kb = rect(p, x + 0.6, y, cw - 0.6, None, t, b, GEN_F, GEN_L,
                  title_size=8.6, body_size=7.4)
        p.arrow([(x + 0.3, root.y + root.h), (x + 0.3, kb.y + kb.h / 2),
                 (kb.x, kb.y + kb.h / 2)], color=DUR, weight=W)
        y = kb.y + kb.h + 0.15
    p.text(x, y + 0.05, cw, "the encoder the derive generated builds this "
           "tree of rows, a RowWrite per row", size=7.8, color=SUB)

    # statements
    x = xs[1]
    st = p.stack(x, 0.95, cw, gap=0.10)
    for i, (t, b) in enumerate([
            ("SAVEPOINT, or BEGIN", "your transaction, or one of its own "
             "(MPA-WRITE-1)"),
            ("INSERT INTO board … ON CONFLICT (id) DO UPDATE",
             "the upsert by key; on MySQL, INSERT … AS new ON DUPLICATE "
             "KEY UPDATE (MPA-WRITE-3)"),
            ("SELECT id FROM list WHERE board_id = ANY($1)",
             "the rows the collection has in the database"),
            ("DELETE the lists that are gone",
             "with their cards first: deepest first (MPA-WRITE-4)"),
            ("upsert the other lists, then their cards",
             "with board_id and position"),
            ("replace the board_label links",
             "delete and insert the link rows (MPA-WRITE-5)"),
            ("RELEASE, or COMMIT", "statements ran as they were called: "
             "nothing waits for a flush")]):
        st.box("%d  %s" % (i + 1, t), b, fill=PLAN_F if 0 < i < 6 else RUN_F,
               line=PLAN_L if 0 < i < 6 else RUN_L, line_weight=LINE,
               title_size=8.4, body_size=7.4, pad=0.12,
               title_mono=False)

    # versions
    x = xs[2]
    u = rect(p, x, 0.95, cw, None,
             "UPDATE board SET …, version = version + 1",
             "WHERE id = $1 AND version = $2: the version it was loaded with",
             PLAN_F, PLAN_L, title_size=8.6)
    one = rect(p, x, u.y + u.h + 0.55, 2.45, None, "one row",
               "saved; the version incremented", OK_F, OK_L, title_size=8.6,
               body_size=7.4)
    none = rect(p, x + 2.75, u.y + u.h + 0.55, 2.45, None, "no row",
                "insert it, unless a row has its key", ROW_F, ORM,
                title_size=8.6, body_size=7.4)
    one.h = none.h = max(one.h, none.h)
    p.arrow([(one.x + one.w / 2, u.y + u.h), (one.x + one.w / 2, one.y)],
            color=CTL, weight=W)
    p.arrow([(none.x + none.w / 2, u.y + u.h), (none.x + none.w / 2, none.y)],
            color=CTL, weight=W)
    ins = rect(p, x, none.y + none.h + 0.55, 2.45, None, "inserted",
               "a new board", OK_F, OK_L, title_size=8.6, body_size=7.4)
    con = rect(p, x + 2.75, none.y + none.h + 0.55, 2.45, None,
               "Error::Conflict",
               "someone changed or deleted it; the transaction is rolled "
               "back", RED_F, RED_L, title_size=8.6, body_size=7.4,
               body_color=RED_T)
    ins.h = con.h = max(ins.h, con.h)
    p.arrow([(none.x + 0.5, none.y + none.h), (none.x + 0.5, none.y + none.h + 0.27),
             (ins.x + ins.w / 2, none.y + none.h + 0.27), (ins.x + ins.w / 2, ins.y)],
            color=CTL, weight=W)
    p.arrow([(con.x + con.w / 2 + 0.3, none.y + none.h),
             (con.x + con.w / 2 + 0.3, con.y)], color=RED_L, weight=W)
    sv = p.stack(x, con.y + con.h + 0.3, cw, gap=0.12)
    for t, b, f, l in [
            ("the insert, if absent", "INSERT … ON CONFLICT (id) DO NOTHING\n"
             "on MySQL, which counts a found row as affected,\n"
             "INSERT … SELECT … WHERE NOT EXISTS (MPA-WRITE-9)", ROW_F, ORM),
            ("written back", "the new versions go into the value and the "
             "elements of its owned collections, so it can be saved again "
             "without reloading (MPA-WRITE-10)", APP_F, APP_L),
            ("delete::<Board, _>(id, tx)", "the row and everything it owns, "
             "deepest first; returns whether it existed (MPA-WRITE-7)",
             PLAN_F, PLAN_L)]:
        sv.box(t, b, fill=f, line=l, line_weight=LINE, title_size=8.8,
               body_size=7.4, pad=0.14)
    return p


# =====================================================================
# 9 · change tracking: Mabat against an ORM
# =====================================================================
def fig_changes(doc):
    p = doc.page("9 Change tracking", 17, 10.6)
    half = 8.1
    xl, xr = 0.25, 0.25 + half + 0.3
    bar(p, xl, 0.3, half, "an ORM  ·  transparent: the session watches the "
        "objects", ORM, size=10)
    bar(p, xr, 0.3, half, "Mabat  ·  explicit: two values, compared when "
        "you save", MABAT, size=10)

    # ---- the ORM ---------------------------------------------------------
    sz = 1.0
    pc = p.box(xl, sz, half, 2.55, "", "", fill="#FCFDFE", line=ORM,
               dashed=True, rounding=0.12, line_weight=0.009)
    p.text(xl + 0.18, sz + 0.1, 6, "the session  ·  persistence context, "
           "EntityManager", size=9.0, color=ORM, bold=True)
    im = rect(p, xl + 0.3, sz + 0.55, 2.4, 0.75, "identity map",
              "one managed object per row it has loaded", ROW_F, ORM,
              title_size=8.8, body_size=7.4)
    sn = rect(p, xl + 2.85, sz + 0.55, 2.4, 0.75, "a snapshot per object",
              "the column values as loaded, kept beside the object",
              ROW_F, ORM, title_size=8.8, body_size=7.4)
    aq = rect(p, xl + 5.4, sz + 0.55, 2.45, 0.75, "the action queue",
              "inserts, updates and deletes waiting for the flush",
              ROW_F, ORM, title_size=8.8, body_size=7.4)
    ob = rect(p, xl + 0.3, sz + 1.55, 7.55, 0.82,
              "your objects are the ORM's: proxies and wrappers",
              "a lazy reference is a generated subclass that loads on first "
              "touch; a collection is replaced by the ORM's own "
              "(PersistentBag) that records adds and removes; enhanced "
              "bytecode can mark fields dirty in their setters",
              APP_F, APP_L, title_size=8.8, body_size=7.4)
    oy = sz + 2.85
    sb = d.steps(p, xl, oy, half, [
        ("load", "objects enter the session, with snapshots"),
        ("change", "task.setTitle(…): no SQL yet"),
        ("flush", "before a query, or at commit: compare every managed "
                  "object with its snapshot"),
        ("SQL", "UPDATEs for the dirty ones, in the queue's order")],
        color=ORM, body_size=7.2)
    jpa = code(p, xl, sb + 0.3, half,
               "Doc doc = em.find(Doc.class, 1L);        // managed, with a snapshot\n"
               "doc.setTitle(\"User guide\");              // no SQL yet\n"
               "doc.getSections().remove(1);            // the wrapped list records it\n"
               "em.createQuery(\"from Doc\").getResultList();  // auto flush: UPDATE, DELETE\n"
               "tx.commit();                            // flushes whatever is left",
               size=7.2)

    # ---- Mabat -------------------------------------------------------------
    c1 = code(p, xr, sz, half,
              "let before = mabat::load::<Doc>().by_key(1).one(&mut tx).await?;\n"
              "let mut after = before.clone();          // plain values\n"
              "after.title = \"User guide\".into();\n"
              "after.sections.remove(1);                // gone\n"
              "after.sections.swap(0, 1);               // moved\n"
              "after.sections[0].heading = \"Questions\".into();\n"
              "after.sections.push(section(4, \"Changes\"));  // new\n"
              "mabat::save_changes(&before, &mut after, &mut tx).await?;",
              size=7.2)
    cmp_ = rect(p, xr, c1.y + c1.h + 0.35, half, None,
                "a comparison the derive generated for Doc",
                "columns and embedded values by PartialEq (a type without it "
                "counts as changed)  ·  references by key  ·  owned "
                "collections element by element, matched by key  ·  links "
                "as lists of keys (MPA-WRITE-8)",
                GEN_F, GEN_L, title_size=8.8, body_size=7.4)
    varrow(p, c1, cmp_, CTL, x=xr + 1.0)
    sq = code(p, xr, cmp_.y + cmp_.h + 0.35, half,
              '-- the title changed\n'
              'UPDATE "doc" SET "title" = $1, "version" = "version" + 1\n'
              '  WHERE "id" = $2 AND "version" = $3\n'
              '-- section 2 is gone: deleted with what it owns\n'
              'DELETE FROM "section" WHERE "id" = ANY($1)\n'
              '-- section 3: a new heading, and moved to position 0\n'
              'UPDATE "section" SET "heading" = $1, "position" = $2,\n'
              '  "version" = "version" + 1 WHERE "id" = $3 AND "version" = $4\n'
              '-- section 1: moved only, so its position\n'
              'UPDATE "section" SET "position" = $1, "version" = "version" + 1 …\n'
              '-- section 4 is new: saved whole, inserted when no row has its key\n'
              'INSERT INTO "section" (…) VALUES (…) ON CONFLICT ("id") DO NOTHING',
              size=7.0)
    varrow(p, cmp_, sq, DUR, x=xr + 1.0)
    p.text(xr, sq.y + sq.h + 0.08, half,
           "one statement per changed row, run now, in a transaction or a "
           "savepoint of yours; a value without changes runs none. A stale "
           "before matches no version: Error::Conflict.",
           size=7.6, color=SUB)

    # ---- side by side --------------------------------------------------------
    y = max(sq.y + sq.h + 0.75, jpa.y + jpa.h + 0.45)
    rows(p, [xl, xr], half, y, [
        ("WHERE THE SNAPSHOT LIVES", [
            "inside the session, for every object it manages, until the "
            "session closes",
            "in your code: before is an ordinary value you keep, clone, "
            "cache, or reload"]),
        ("HOW A CHANGE IS NOTICED", [
            "at flush, by comparing each managed object with its snapshot, "
            "or by flags set in enhanced setters",
            "when save_changes is called, by generated code comparing before "
            "with after; no proxies, no wrappers, no bytecode"]),
        ("WHEN SQL RUNS", [
            "when the session decides: before a query that might see the "
            "change (auto flush), or at commit",
            "when you call save_changes, in your transaction or a "
            "savepoint of it"]),
        ("ACROSS REQUESTS", [
            "a detached object must be merged, which reloads it to have a "
            "snapshot again",
            "keep before (or reload it); #[view(version)] makes a stale one "
            "fail with Error::Conflict (MPA-WRITE-9)"]),
        ("WHAT CAN SURPRISE YOU", [
            "a flush you did not ask for; a lazy load after the session "
            "closed; an N+1 behind a getter",
            "nothing is saved unless you call it; a before that is not what "
            "was loaded writes the wrong difference, unless versioned"]),
        ("IN RUST", [
            "no runtime proxies: SeaORM makes tracking explicit with "
            "ActiveModel's Set and Unchanged values; Diesel has none",
            "the structs stay plain: Clone and PartialEq are your choice, "
            "and the view is still a view for reading"]),
    ])
    return p


# =====================================================================
# 10 · the contract, from docs/mpa.json
# =====================================================================
AREAS = ["CORE", "DB", "VIEW", "SUM", "LOAD", "PLAN", "OVR", "JSON", "GQL",
         "WRITE"]


def load_index():
    with open(os.path.join(_repo, "docs", "mpa.json"), encoding="utf-8") as fh:
        return json.load(fh)


def fig_contract(doc):
    index = load_index()
    caps = index["capabilities"]
    rules = [r["id"] for r in index["rules"]]
    per_area = {a: sum(1 for r in rules if r.split("-")[1] == a) for a in AREAS}

    p = doc.page("10 The contract", 17, 10)
    nw, sw = 2.15, 5.05
    aw = (16.5 - nw - sw) / len(AREAS)
    x0 = 0.25
    rh = 0.29
    y = 0.3
    p.box(x0, y, nw + sw, 0.52, "capability, from docs/mpa.json", "",
          fill=MABAT, line=MABAT, title_size=9.0, title_color=PAPER,
          valign=1, rounding=0.03)
    for i, a in enumerate(AREAS):
        p.box(x0 + nw + sw + i * aw, y, aw, 0.52, a,
              "%d rules" % per_area[a], fill=MABAT, line=MABAT,
              title_size=8.6, body_size=6.8, title_color=PAPER,
              body_color="#D9D7F7", halign=1, valign=1, rounding=0.03)
    y += 0.58
    cited = set()
    for n, c in enumerate(caps):
        band = "#F7F7FD" if n % 2 == 0 else PAPER
        p.box(x0, y, nw, rh, c["id"], "", fill=band, line=ROW_L,
              line_weight=0.006, title_size=7.6, valign=1, rounding=0,
              title_color=INK)
        p.box(x0 + nw, y, sw, rh, c["summary"], "", fill=band, line=ROW_L,
              line_weight=0.006, title_size=6.9, title_bold=False, valign=1,
              rounding=0, title_color=SUB)
        for i, a in enumerate(AREAS):
            nums = [r.split("-")[2] for r in c["rules"]
                    if r.split("-")[1] == a]
            cited.update(r for r in c["rules"])
            p.box(x0 + nw + sw + i * aw, y, aw, rh, ", ".join(nums), "",
                  fill=MABAT_F if nums else band, line=ROW_L,
                  line_weight=0.006, title_size=7.4, title_color=MABAT,
                  halign=1, valign=1, rounding=0)
        y += rh
    unknown = sorted(cited - set(rules))
    if unknown:
        raise SystemExit("docs/mpa.json cites rules it does not define: %s"
                         % ", ".join(unknown))
    covered = [r for r in rules if r.split("-")[1] in AREAS]
    loose = [r for r in covered if r not in cited]
    p.text(x0, y + 0.15, 16.5,
           "%d capabilities, %d rules in %d areas, plus %d conventions "
           "(DOC) and %d things Mabat 0.1 does not do (NOT). A number is a "
           "rule of its column's area: VIEW 9 is MPA-VIEW-9. %d rules of these "
           "areas define details no capability names on its own."
           % (len(caps), len(covered), len(AREAS),
              sum(1 for r in rules if "-DOC-" in r),
              len(index["unsupported"]), len(loose)),
           size=7.8, color=SUB)
    return p


FIGS = [("01-at-a-glance", fig_glance),
        ("02-view-shape", fig_shape),
        ("03-plan", fig_plan),
        ("04-overrides", fig_overrides),
        ("05-result-shapes", fig_results),
        ("06-concurrency", fig_concurrency),
        ("07-serving", fig_serving),
        ("08-writes", fig_writes),
        ("09-change-tracking", fig_changes),
        ("10-contract", fig_contract)]


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    outdir = args[0] if args else _here
    doc = k.Document(
        title="Mabat — architecture",
        creator="mabat",
        description="Ten figures: Mabat at a glance; a view as a shape; the "
                    "plan of a view; overrides; the shapes of a result; "
                    "concurrency; JSON and GraphQL; writes; change tracking "
                    "against an ORM; and the contract from docs/mpa.json.")
    for _, fn in FIGS:
        fn(doc).trim(margin=0.3)

    problems = (doc.check() + doc.overlap_report()
                + doc.label_overlap_report() + doc.label_on_line_report())
    for pg in doc.pages:
        problems += (d.ink_collision_report(pg) + d.edge_strike_report(pg)
                     + d.arrow_through_text_report(pg))
    for msg in problems:
        print(msg)
    print("%d shapes, %d pages, %d problems"
          % (sum(len(pg.shapes) for pg in doc.pages), len(doc.pages),
             len(problems)))

    out = os.path.join(outdir, "mabat-architecture.vsdx")
    doc.save(out)
    print("wrote", out)

    if "--svg" in sys.argv:
        ddir = os.path.join(outdir, "diagrams")
        os.makedirs(ddir, exist_ok=True)
        for (stem, _), pg in zip(FIGS, doc.pages):
            path = os.path.join(ddir, stem + ".svg")
            with open(path, "w") as fh:
                fh.write(k._page_svg(pg))
            print("wrote", path)

    if "--emf" in sys.argv:
        for path in emf.save_emf(doc, os.path.join(outdir,
                                                   "mabat-architecture")):
            problems += ["%s: %s" % (path, m) for m in emf.validate_emf(path)]
            print("wrote", path)

    if "--pdf" in sys.argv:
        chrome = d.find_chrome()
        if not chrome:
            raise SystemExit("no Chrome/Chromium found")
        svgs = doc.save_svg(os.path.join(outdir, "preview"))
        pdf = d.render_pdf(doc, svgs, os.path.join(
            outdir, "mabat-figures.pdf"), chrome)
        for msg in d.check_pdf(doc, pdf):
            print("  PDF CHECK:", msg)
            problems.append(msg)
        for s in svgs:
            if os.path.exists(s):
                os.remove(s)
        print("wrote", pdf)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
