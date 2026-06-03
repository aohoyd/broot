use {
    crate::tree::TreeLineType,
    crokey::crossterm::style::Color,
    once_cell::sync::Lazy,
    rustc_hash::FxHashMap,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileClass {
    Directory,
    Code,
    Config,
    Document,
    Image,
    Audio,
    Video,
    Archive,
    Font,
    Data,
    File,
    Other,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb { r, g, b }
}

// Effective resolved palette from elio:
//   classes.rs (Rust baseline)
// + extensions.rs / files.rs (Rust baseline)
// + theme.toml (final overlay — wins on conflict)
//
// Keys are lowercased; lookups lowercase before matching.

#[rustfmt::skip]
static CLASS_COLOR_PAIRS: &[(FileClass, (u8, u8, u8))] = &[
    (FileClass::Directory, (91, 168, 255)),  // #5ba8ff
    (FileClass::Code,      (56, 213, 255)),  // #38d5ff
    (FileClass::Config,    (179, 140, 255)), // #b38cff
    (FileClass::Document,  (141, 223, 109)), // #8ddf6d
    (FileClass::Image,     (36, 217, 184)),  // #24d9b8
    (FileClass::Audio,     (247, 200, 94)),  // #f7c85e
    (FileClass::Video,     (255, 134, 216)), // #ff86d8
    (FileClass::Archive,   (207, 111, 63)),  // #cf6f3f
    (FileClass::Font,      (215, 142, 255)), // #d78eff
    (FileClass::Data,      (89, 222, 148)),  // #59de94
    (FileClass::File,      (214, 222, 240)), // #d6def0
    (FileClass::Other,     (170, 170, 170)),
];

#[rustfmt::skip]
static EXT_COLOR_PAIRS: &[(&str, (u8, u8, u8))] = &[
    ("rs",      (255, 143, 64)),   // #ff8f40 (TOML)
    ("js",      (103, 176, 255)),  // #67b0ff
    ("mjs",     (103, 176, 255)),  // #67b0ff
    ("cjs",     (103, 176, 255)),  // #67b0ff
    ("ts",      (90, 168, 255)),   // #5aa8ff
    ("mts",     (90, 168, 255)),   // #5aa8ff
    ("cts",     (90, 168, 255)),   // #5aa8ff
    ("tsx",     (109, 196, 255)),  // #6dc4ff
    ("jsx",     (130, 203, 255)),  // #82cbff
    ("py",      (255, 216, 102)),  // #ffd866
    ("go",      (102, 217, 239)),  // #66d9ef
    ("c",       (121, 184, 255)),  // #79b8ff
    ("h",       (121, 184, 255)),  // #79b8ff
    ("cpp",     (140, 184, 255)),  // #8cb8ff
    ("hpp",     (140, 184, 255)),  // #8cb8ff
    ("cs",      (104, 179, 120)),  // (extensions.rs)
    ("csx",     (104, 179, 120)),
    ("dart",    (56, 213, 255)),   // #38d5ff (TOML)
    ("java",    (255, 143, 97)),   // #ff8f61
    ("lua",     (122, 174, 255)),  // #7aaeff
    ("php",     (179, 140, 255)),  // #b38cff
    ("rb",      (255, 123, 114)),  // #ff7b72
    ("swift",   (255, 155, 97)),   // #ff9b61
    ("kt",      (199, 146, 234)),  // #c792ea
    ("vue",     (89, 222, 148)),   // #59de94
    ("svelte",  (255, 155, 97)),   // #ff9b61
    ("astro",   (214, 222, 240)),  // #d6def0
    ("html",    (255, 155, 97)),   // #ff9b61
    ("htm",     (255, 155, 97)),   // #ff9b61
    ("css",     (47, 225, 200)),   // #2fe1c8
    ("scss",    (207, 136, 255)),  // #cf88ff
    ("xml",     (179, 140, 255)),  // #b38cff
    ("xsd",     (179, 140, 255)),
    ("xsl",     (179, 140, 255)),
    ("xslt",    (179, 140, 255)),
    ("zig",     (247, 200, 94)),   // #f7c85e
    ("qml",     (64, 205, 82)),    // (extensions.rs)
    ("diff",    (255, 184, 107)),
    ("patch",   (255, 184, 107)),
    ("groovy",  (112, 182, 117)),
    ("gvy",     (112, 182, 117)),
    ("scala",   (232, 90, 90)),
    ("pl",      (125, 176, 255)),
    ("pm",      (125, 176, 255)),
    ("pod",     (125, 176, 255)),
    ("hs",      (179, 140, 255)),
    ("lhs",     (179, 140, 255)),
    ("jl",      (193, 120, 255)),
    ("r",       (95, 153, 219)),
    ("ex",      (155, 143, 199)),
    ("exs",     (155, 143, 199)),
    ("clj",     (128, 176, 92)),
    ("cljs",    (128, 176, 92)),
    ("cljc",    (128, 176, 92)),
    ("edn",     (128, 176, 92)),
    ("ps1",     (95, 153, 219)),
    ("psm1",    (95, 153, 219)),
    ("psd1",    (95, 153, 219)),
    ("sh",      (214, 222, 240)),  // #d6def0
    ("bash",    (214, 222, 240)),
    ("zsh",     (214, 222, 240)),
    ("fish",    (214, 222, 240)),
    ("f",       (115, 79, 150)),
    ("for",     (115, 79, 150)),
    ("f90",     (115, 79, 150)),
    ("f95",     (115, 79, 150)),
    ("f03",     (115, 79, 150)),
    ("f08",     (115, 79, 150)),
    ("fpp",     (115, 79, 150)),
    ("cbl",     (0, 92, 165)),
    ("cob",     (0, 92, 165)),
    ("cobol",   (0, 92, 165)),
    ("cpy",     (0, 92, 165)),

    ("json",    (125, 176, 255)),  // #7db0ff
    ("jsonc",   (125, 176, 255)),
    ("json5",   (125, 176, 255)),
    ("yaml",    (179, 140, 255)),
    ("yml",     (179, 140, 255)),
    ("toml",    (179, 140, 255)),
    ("ini",     (179, 140, 255)),
    ("conf",    (179, 140, 255)),
    ("cfg",     (179, 140, 255)),
    ("desktop", (125, 176, 255)),  // #7db0ff (TOML)
    ("ron",     (179, 140, 255)),
    ("env",     (179, 140, 255)),
    ("nix",     (122, 174, 255)),  // #7aaeff
    ("hcl",     (179, 140, 255)),
    ("tf",      (179, 140, 255)),
    ("tfvars",  (179, 140, 255)),
    ("tfbackend", (179, 140, 255)),
    ("gradle",  (112, 182, 117)),
    ("sbt",     (232, 90, 90)),
    ("just",    (255, 184, 107)),
    ("ziggy",   (245, 173, 64)),
    ("keys",    (255, 216, 102)),  // #ffd866
    ("key",     (255, 216, 102)),
    ("p12",     (224, 180, 91)),   // #e0b45b
    ("pfx",     (224, 180, 91)),
    ("pem",     (220, 184, 95)),   // #dcb85f
    ("crt",     (220, 184, 95)),
    ("cer",     (220, 184, 95)),
    ("csr",     (201, 174, 108)),  // #c9ae6c

    ("md",       (211, 170, 124)), // #d3aa7c
    ("markdown", (211, 170, 124)),
    ("mdown",    (211, 170, 124)),
    ("mkd",      (211, 170, 124)),
    ("mdx",      (211, 170, 124)),
    ("txt",      (174, 184, 199)), // #aeb8c7
    ("rst",      (141, 223, 109)),
    ("pdf",      (255, 107, 107)), // #ff6b6b (TOML)
    ("epub",     (211, 170, 124)), // #d3aa7c (rule_ebook_file)
    ("mobi",     (211, 170, 124)),
    ("azw3",     (211, 170, 124)),
    ("doc",      (88, 142, 255)),
    ("docx",     (88, 142, 255)),
    ("docm",     (88, 142, 255)),
    ("odt",      (88, 142, 255)),
    ("pages",    (88, 142, 255)),
    ("ods",      (78, 178, 116)),
    ("xlsx",     (78, 178, 116)),
    ("xlsm",     (78, 178, 116)),
    ("odp",      (232, 139, 63)),
    ("pptx",     (232, 139, 63)),
    ("pptm",     (232, 139, 63)),
    ("log",      (140, 151, 168)), // #8c97a8
    ("srt",      (141, 223, 109)), // #8ddf6d

    ("png",  (36, 217, 184)),  // #24d9b8 (TOML)
    ("jpg",  (36, 217, 184)),
    ("jpeg", (36, 217, 184)),
    ("gif",  (36, 217, 184)),
    ("svg",  (36, 217, 184)),
    ("webp", (36, 217, 184)),
    ("avif", (36, 217, 184)),
    ("xcf",  (255, 159, 122)), // #ff9f7a
    ("ico",  (121, 198, 255)), // #79c6ff

    ("mp3",  (247, 200, 94)),
    ("wav",  (247, 200, 94)),
    ("flac", (247, 200, 94)),
    ("ogg",  (247, 200, 94)),
    ("m4a",  (247, 200, 94)),

    ("mp4",  (255, 134, 216)),
    ("mkv",  (255, 134, 216)),
    ("mov",  (255, 134, 216)),
    ("webm", (255, 134, 216)),
    ("avi",  (255, 134, 216)),

    ("zip",      (207, 111, 63)),
    ("tar",      (207, 111, 63)),
    ("gz",       (207, 111, 63)),
    ("xz",       (207, 111, 63)),
    ("bz2",      (207, 111, 63)),
    ("7z",       (207, 111, 63)),
    ("iso",      (184, 190, 200)), // #b8bec8
    ("rpm",      (139, 30, 45)),   // #8b1e2d
    ("deb",      (255, 107, 134)), // #ff6b86
    ("apk",      (103, 212, 111)), // #67d46f
    ("aab",      (86, 199, 217)),  // #56c7d9
    ("apkg",     (90, 168, 255)),  // #5aa8ff
    ("zst",      (200, 168, 106)), // #c8a86a
    ("zest",     (255, 184, 107)), // #ffb86b
    ("appimage", (102, 187, 255)), // #66bbff
    ("jar",      (255, 143, 97)),  // #ff8f61
    ("cbz",      (211, 170, 124)), // (extensions.rs)
    ("cbr",      (211, 170, 124)),

    ("ttf",   (215, 142, 255)),
    ("otf",   (215, 142, 255)),
    ("woff",  (215, 142, 255)),
    ("woff2", (215, 142, 255)),

    ("csv",     (89, 222, 148)),
    ("tsv",     (89, 222, 148)),
    ("sqlite",  (89, 222, 148)),
    ("sqlite3", (89, 222, 148)),
    ("db3",     (89, 222, 148)),
    ("db",      (89, 222, 148)),
    ("parquet", (89, 222, 148)),
    ("torrent", (127, 212, 106)),  // #7fd46a
    ("hash",    (141, 214, 255)),  // #8dd6ff
    ("sha1",    (141, 223, 109)),
    ("sha256",  (141, 223, 109)),
    ("sha512",  (141, 223, 109)),
    ("md5",     (141, 223, 109)),
    ("sql",     (89, 222, 148)),   // #59de94 (TOML)
    ("lock",    (89, 222, 148)),   // #59de94

    ("exe",  (125, 176, 255)),     // #7db0ff (TOML)
];

#[rustfmt::skip]
static FILENAME_COLOR_PAIRS: &[(&str, (u8, u8, u8))] = &[
    // Broot-specific: tan rather than toml's purple — Cargo.toml is a
    // doc-ish anchor in Rust projects, not just another config file.
    ("cargo.toml",          (211, 170, 124)),
    // From elio TOML [files.*]
    ("cargo.lock",          (89, 222, 148)),   // #59de94
    ("package.json",        (125, 176, 255)),  // #7db0ff
    ("package-lock.json",   (89, 222, 148)),
    ("tsconfig.json",       (168, 132, 255)),  // #a884ff
    ("dockerfile",          (78, 207, 255)),   // #4ecfff
    ("containerfile",       (78, 207, 255)),
    ("compose.yml",         (40, 213, 194)),   // #28d5c2
    ("compose.yaml",        (40, 213, 194)),
    ("docker-compose.yml",  (40, 213, 194)),
    ("docker-compose.yaml", (40, 213, 194)),
    ("readme.md",           (211, 170, 124)),  // #d3aa7c
    ("authors",             (155, 143, 199)),  // #9b8fc7
    ("authors.md",          (155, 143, 199)),
    ("authors.txt",         (155, 143, 199)),
    ("contributors",        (155, 143, 199)),
    ("contributors.md",     (155, 143, 199)),
    (".gitignore",          (215, 142, 255)),  // #d78eff
    (".gitattributes",      (215, 142, 255)),
    (".gitmodules",         (215, 142, 255)),
    (".env",                (102, 227, 154)),  // #66e39a
    (".npmrc",              (255, 123, 114)),  // #ff7b72
    (".yarnrc.yml",         (36, 217, 184)),   // #24d9b8
    ("pnpm-lock.yaml",      (255, 184, 107)),  // #ffb86b
    ("yarn.lock",           (36, 217, 184)),
    ("bun.lock",            (247, 200, 94)),   // #f7c85e
    ("bun.lockb",           (247, 200, 94)),
    ("pnpm-workspace.yaml", (255, 184, 107)),
    ("biome.json",          (78, 207, 255)),
    ("turbo.json",          (214, 222, 240)),  // #d6def0
    ("deno.json",           (102, 227, 154)),
    ("deno.jsonc",          (102, 227, 154)),
    ("poetry.lock",         (141, 223, 109)),
    ("requirements.txt",    (91, 168, 255)),   // #5ba8ff
    ("go.mod",              (78, 207, 255)),
    ("go.sum",              (102, 227, 154)),
    ("go.work",             (78, 207, 255)),
    ("go.work.sum",         (102, 227, 154)),
    ("vite.config.ts",      (215, 142, 255)),
    ("vite.config.js",      (215, 142, 255)),
    ("vite.config.mjs",     (215, 142, 255)),
    ("vite.config.mts",     (215, 142, 255)),
    ("tailwind.config.ts",  (56, 213, 255)),
    ("tailwind.config.js",  (56, 213, 255)),
    ("tailwind.config.mjs", (56, 213, 255)),
    ("eslint.config.js",    (179, 140, 255)),
    ("eslint.config.mjs",   (179, 140, 255)),
    ("eslint.config.ts",    (179, 140, 255)),
    ("prettier.config.js",  (255, 134, 216)),
    ("prettier.config.mjs", (255, 134, 216)),
    ("prettier.config.ts",  (255, 134, 216)),
    ("postcss.config.js",   (47, 225, 200)),
    ("postcss.config.cjs",  (47, 225, 200)),
    ("jest.config.js",      (247, 200, 94)),
    ("jest.config.ts",      (247, 200, 94)),
    ("vitest.config.ts",    (141, 223, 109)),
    ("vitest.config.js",    (141, 223, 109)),
    ("svelte.config.js",    (255, 155, 97)),
    ("astro.config.mjs",    (214, 222, 240)),
    ("nuxt.config.ts",      (89, 222, 148)),
    ("nuxt.config.js",      (89, 222, 148)),
    (".prettierrc",         (255, 134, 216)),
    (".prettierignore",     (255, 134, 216)),
    (".editorconfig",       (214, 222, 240)),
    ("makefile",            (255, 155, 97)),
    ("pkgbuild",            (102, 187, 255)),
    ("justfile",            (247, 200, 94)),
    (".justfile",           (255, 184, 107)),
    ("flake.nix",           (91, 168, 255)),
    (".dockerignore",       (78, 207, 255)),
    (".node-version",       (141, 223, 109)),
    (".nvmrc",              (141, 223, 109)),
    (".python-version",     (255, 216, 102)),
    ("pipfile",             (255, 216, 102)),
    ("pipfile.lock",        (89, 222, 148)),
    ("uv.lock",             (89, 222, 148)),
    ("next.config.js",      (214, 222, 240)),
    ("next.config.ts",      (214, 222, 240)),
    (".terraform.lock.hcl", (179, 140, 255)),
    ("build.gradle",        (112, 182, 117)),
    ("settings.gradle",     (112, 182, 117)),
    ("init.gradle",         (112, 182, 117)),
    ("build.sbt",           (232, 90, 90)),
    (".rprofile",           (95, 153, 219)),
    ("project.clj",         (128, 176, 92)),
    ("deps.edn",            (128, 176, 92)),
    ("bb.edn",              (128, 176, 92)),
    ("shadow-cljs.edn",     (128, 176, 92)),
    ("build.zig.zon",       (245, 173, 64)),
    // License: elio sniffs file content for these; we match by name instead.
    ("license",       (245, 216, 91)),
    ("license.md",    (245, 216, 91)),
    ("license.txt",   (245, 216, 91)),
    ("licence",       (245, 216, 91)),
    ("licence.md",    (245, 216, 91)),
    ("licence.txt",   (245, 216, 91)),
    ("copying",       (245, 216, 91)),
    ("copying.md",    (245, 216, 91)),
    ("copying.txt",   (245, 216, 91)),
    ("unlicense",     (245, 216, 91)),
];

#[rustfmt::skip]
static DIRNAME_COLOR_PAIRS: &[(&str, (u8, u8, u8))] = &[
    (".git",         (138, 146, 168)), // #8a92a8
    (".config",      (179, 140, 255)),
    (".github",      (214, 222, 240)),
    (".vscode",      (91, 168, 255)),
    (".idea",        (255, 134, 216)),
    (".cargo",       (255, 143, 64)),
    (".ssh",         (214, 222, 240)),
    (".cache",       (138, 146, 168)),
    (".npm",         (141, 223, 109)),
    (".yarn",        (36, 217, 184)),
    (".pnpm-store",  (255, 184, 107)),
    (".venv",        (255, 216, 102)),
    ("venv",         (255, 216, 102)),
    ("node_modules", (91, 168, 255)),
    ("tests",        (91, 168, 255)),
    ("test",         (91, 168, 255)),
    ("__tests__",    (91, 168, 255)),
    ("scripts",      (91, 168, 255)),
    ("build",        (91, 168, 255)),
    ("dist",         (91, 168, 255)),
    (".next",        (91, 168, 255)),
    (".nuxt",        (91, 168, 255)),
    (".svelte-kit",  (91, 168, 255)),
    (".astro",       (91, 168, 255)),
    ("public",       (91, 168, 255)),
    ("publico",      (91, 168, 255)),
    ("público",      (91, 168, 255)),
    ("pictures",     (36, 217, 184)),
    ("imagenes",     (36, 217, 184)),
    ("imágenes",     (36, 217, 184)),
    ("documents",    (141, 223, 109)),
    ("documentos",   (141, 223, 109)),
    ("downloads",    (247, 200, 94)),
    ("descargas",    (247, 200, 94)),
    ("music",        (215, 142, 255)),
    ("musica",       (215, 142, 255)),
    ("música",       (215, 142, 255)),
    ("videos",       (255, 134, 216)),
    ("vídeos",       (255, 134, 216)),
    ("desktop",      (125, 176, 255)),
    ("escritorio",   (125, 176, 255)),
    ("assets",       (91, 168, 255)),
    ("coverage",     (91, 168, 255)),
    ("tmp",          (91, 168, 255)),
    ("temp",         (91, 168, 255)),
    ("out",          (91, 168, 255)),
    ("target",       (91, 168, 255)),
    ("bin",          (91, 168, 255)),
    ("lib",          (91, 168, 255)),
    ("vendor",       (91, 168, 255)),
    ("src",          (91, 168, 255)),
    ("config",       (91, 168, 255)),
    ("docs",         (91, 168, 255)),
];

static EXT_MAP: Lazy<FxHashMap<&'static str, Color>> = Lazy::new(|| {
    EXT_COLOR_PAIRS
        .iter()
        .map(|(k, (r, g, b))| (*k, rgb(*r, *g, *b)))
        .collect()
});

static FILENAME_MAP: Lazy<FxHashMap<&'static str, Color>> = Lazy::new(|| {
    FILENAME_COLOR_PAIRS
        .iter()
        .map(|(k, (r, g, b))| (*k, rgb(*r, *g, *b)))
        .collect()
});

static DIRNAME_MAP: Lazy<FxHashMap<&'static str, Color>> = Lazy::new(|| {
    DIRNAME_COLOR_PAIRS
        .iter()
        .map(|(k, (r, g, b))| (*k, rgb(*r, *g, *b)))
        .collect()
});

static CLASS_MAP: Lazy<FxHashMap<FileClass, Color>> = Lazy::new(|| {
    CLASS_COLOR_PAIRS
        .iter()
        .map(|(k, (r, g, b))| (*k, rgb(*r, *g, *b)))
        .collect()
});

// CLASS_COLOR_PAIRS is exhaustive over FileClass; the expect message
// pins that invariant so a future variant addition cannot silently
// produce a misleading fallback.
fn class_color(class: FileClass) -> Color {
    *CLASS_MAP
        .get(&class)
        .expect("CLASS_COLOR_PAIRS must cover every FileClass variant")
}

fn ext_color(ext: &str) -> Option<Color> {
    let lower = ext.to_ascii_lowercase();
    EXT_MAP.get(lower.as_str()).copied()
}

fn filename_color(name: &str) -> Option<Color> {
    let lower = name.to_ascii_lowercase();
    FILENAME_MAP.get(lower.as_str()).copied()
}

// Lookup uses `to_lowercase` (not ascii) so non-ASCII uppercase names
// such as `Público`, `Imágenes`, `Música` fold to their table keys.
fn dirname_color(name: &str) -> Option<Color> {
    let lower = name.to_lowercase();
    DIRNAME_MAP.get(lower.as_str()).copied()
}

pub fn infer_class(ext: Option<&str>) -> FileClass {
    let Some(ext) = ext else {
        return FileClass::Other;
    };
    let lower = ext.to_ascii_lowercase();
    match lower.as_str() {
        "rs" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "tsx" | "jsx"
        | "py" | "go" | "c" | "h" | "cpp" | "hpp" | "cs" | "csx" | "dart"
        | "java" | "lua" | "php" | "rb" | "swift" | "kt" | "vue" | "svelte"
        | "astro" | "html" | "htm" | "css" | "scss" | "xml" | "xsd" | "xsl"
        | "xslt" | "zig" | "qml" | "diff" | "patch" | "groovy" | "gvy"
        | "scala" | "pl" | "pm" | "pod" | "hs" | "lhs" | "jl" | "r"
        | "ex" | "exs" | "clj" | "cljs" | "cljc" | "edn"
        | "ps1" | "psm1" | "psd1"
        | "sh" | "bash" | "zsh" | "fish"
        | "f" | "for" | "f90" | "f95" | "f03" | "f08" | "fpp"
        | "cbl" | "cob" | "cobol" | "cpy" => FileClass::Code,

        "json" | "jsonc" | "json5" | "yaml" | "yml" | "toml" | "ini" | "conf"
        | "cfg" | "desktop" | "ron" | "env" | "nix" | "hcl" | "tf" | "tfvars"
        | "tfbackend" | "gradle" | "sbt" | "just" | "ziggy"
        | "keys" | "key" | "p12" | "pfx" | "pem" | "crt" | "cer" | "csr" => {
            FileClass::Config
        }

        "md" | "markdown" | "mdown" | "mkd" | "mdx" | "txt" | "rst" | "pdf"
        | "epub" | "mobi" | "azw3" | "doc" | "docx" | "docm" | "odt" | "ods"
        | "xlsx" | "xlsm" | "odp" | "pptx" | "pptm" | "pages" | "log" | "srt" => {
            FileClass::Document
        }

        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "avif" | "xcf"
        | "ico" => FileClass::Image,

        "mp3" | "wav" | "flac" | "ogg" | "m4a" => FileClass::Audio,

        "mp4" | "mkv" | "mov" | "webm" | "avi" => FileClass::Video,

        "zip" | "tar" | "gz" | "xz" | "bz2" | "7z" | "iso" | "rpm" | "deb"
        | "apk" | "aab" | "apkg" | "zst" | "jar" | "zest" | "appimage"
        | "cbz" | "cbr" => FileClass::Archive,

        "ttf" | "otf" | "woff" | "woff2" => FileClass::Font,

        "csv" | "tsv" | "sqlite" | "sqlite3" | "db3" | "db" | "parquet"
        | "torrent" | "hash" | "sha1" | "sha256" | "sha512" | "md5" | "sql"
        | "lock" => FileClass::Data,

        _ => FileClass::Other,
    }
}

pub fn resolve(
    line_type: &TreeLineType,
    name: &str,
    ext: Option<&str>,
) -> Option<Color> {
    match line_type {
        TreeLineType::Pruning => None,
        TreeLineType::Dir => Some(
            dirname_color(name).unwrap_or_else(|| class_color(FileClass::Directory)),
        ),
        TreeLineType::File => Some(resolve_file(name, ext)),
        TreeLineType::SymLink { final_target, .. } => {
            let target_name = final_target
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(name);
            let target_ext = final_target
                .extension()
                .and_then(|e| e.to_str());
            Some(resolve_file(target_name, target_ext))
        }
        TreeLineType::BrokenSymLink(_) => Some(class_color(FileClass::Other)),
    }
}

fn resolve_file(name: &str, ext: Option<&str>) -> Color {
    if let Some(c) = filename_color(name) {
        return c;
    }
    if let Some(e) = ext {
        if let Some(c) = ext_color(e) {
            return c;
        }
    }
    class_color(infer_class(ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::TreeLineType;
    use crokey::crossterm::style::Color;
    use std::path::PathBuf;

    fn rgb(r: u8, g: u8, b: u8) -> Color { Color::Rgb { r, g, b } }

    #[test]
    fn rs_extension_resolves_to_orange() {
        let c = resolve(&TreeLineType::File, "main.rs", Some("rs"));
        assert_eq!(c, Some(rgb(255, 143, 64)));
    }

    #[test]
    fn cargo_toml_filename_wins_over_ext() {
        let c = resolve(&TreeLineType::File, "Cargo.toml", Some("toml"));
        assert_eq!(c, Some(rgb(211, 170, 124)));
    }

    #[test]
    fn unknown_extension_falls_back_to_class_other() {
        let c = resolve(&TreeLineType::File, "weird.xyzzy", Some("xyzzy"));
        assert_eq!(c, Some(rgb(170, 170, 170)));
    }

    #[test]
    fn dir_resolves_by_name() {
        let c = resolve(&TreeLineType::Dir, ".git", None);
        assert_eq!(c, Some(rgb(138, 146, 168)));
    }

    #[test]
    fn dir_falls_back_to_directory_class_color() {
        let c = resolve(&TreeLineType::Dir, "RandomDir", None);
        assert_eq!(c, Some(rgb(91, 168, 255)));
    }

    #[test]
    fn pruning_resolves_to_none() {
        let c = resolve(&TreeLineType::Pruning, "...", None);
        assert_eq!(c, None);
    }

    #[test]
    fn symlink_resolves_via_target_name() {
        let c = resolve(
            &TreeLineType::SymLink {
                direct_target: "main.rs".into(),
                final_is_dir: false,
                final_target: PathBuf::from("main.rs"),
            },
            "link",
            Some("rs"),
        );
        assert_eq!(c, Some(rgb(255, 143, 64)));
    }

    #[test]
    fn broken_symlink_resolves_to_other_class() {
        let c = resolve(&TreeLineType::BrokenSymLink("gone".into()), "link", None);
        assert_eq!(c, Some(rgb(170, 170, 170)));
    }

    #[test]
    fn infer_class_recognizes_code_extensions() {
        assert!(matches!(infer_class(Some("rs")), FileClass::Code));
    }

    #[test]
    fn infer_class_recognizes_markdown() {
        assert!(matches!(infer_class(Some("md")), FileClass::Document));
    }

    #[test]
    fn infer_class_falls_back_to_other() {
        assert!(matches!(infer_class(Some("xyzzy")), FileClass::Other));
    }

    #[test]
    fn infer_class_none_ext_falls_back_to_other() {
        assert!(matches!(infer_class(None), FileClass::Other));
    }

    #[test]
    fn py_extension_resolves_to_yellow() {
        assert_eq!(
            resolve(&TreeLineType::File, "main.py", Some("py")),
            Some(rgb(255, 216, 102)),
        );
    }

    #[test]
    fn go_extension_resolves() {
        let c = resolve(&TreeLineType::File, "main.go", Some("go"));
        assert_eq!(c, Some(rgb(102, 217, 239)));
    }

    #[test]
    fn md_extension_resolves_to_tan() {
        // `notes.md` is not in FILENAME_MAP, so this exercises the
        // extension-lookup path (unlike `README.md`, which hits the
        // filename map first).
        assert_eq!(
            resolve(&TreeLineType::File, "notes.md", Some("md")),
            Some(rgb(211, 170, 124)),
        );
    }

    #[test]
    fn json_extension_resolves() {
        let c = resolve(&TreeLineType::File, "config.json", Some("json"));
        assert_eq!(c, Some(rgb(125, 176, 255)));
    }

    #[test]
    fn yaml_extension_resolves() {
        let c = resolve(&TreeLineType::File, "ci.yaml", Some("yaml"));
        assert_eq!(c, Some(rgb(179, 140, 255)));
    }

    #[test]
    fn sh_extension_resolves() {
        let c = resolve(&TreeLineType::File, "run.sh", Some("sh"));
        assert_eq!(c, Some(rgb(214, 222, 240)));
    }

    #[test]
    fn lock_extension_resolves() {
        let c = resolve(&TreeLineType::File, "deps.lock", Some("lock"));
        assert_eq!(c, Some(rgb(89, 222, 148)));
    }

    #[test]
    fn xml_extension_resolves() {
        let c = resolve(&TreeLineType::File, "doc.xml", Some("xml"));
        assert_eq!(c, Some(rgb(179, 140, 255)));
    }

    #[test]
    fn pdf_extension_resolves() {
        let c = resolve(&TreeLineType::File, "paper.pdf", Some("pdf"));
        assert_eq!(c, Some(rgb(255, 107, 107)));
    }

    #[test]
    fn dart_extension_resolves() {
        let c = resolve(&TreeLineType::File, "main.dart", Some("dart"));
        assert_eq!(c, Some(rgb(56, 213, 255)));
    }

    #[test]
    fn license_filename_resolves() {
        let c = resolve(&TreeLineType::File, "LICENSE", None);
        assert_eq!(c, Some(rgb(245, 216, 91)));
    }

    #[test]
    fn dockerfile_filename_resolves() {
        let c = resolve(&TreeLineType::File, "Dockerfile", None);
        assert_eq!(c, Some(rgb(78, 207, 255)));
    }

    #[test]
    fn node_modules_dir_resolves() {
        let c = resolve(&TreeLineType::Dir, "node_modules", None);
        assert_eq!(c, Some(rgb(91, 168, 255)));
    }

    #[test]
    fn src_dir_resolves() {
        let c = resolve(&TreeLineType::Dir, "src", None);
        assert_eq!(c, Some(rgb(91, 168, 255)));
    }

    #[test]
    fn downloads_dir_resolves() {
        let c = resolve(&TreeLineType::Dir, "Downloads", None);
        assert_eq!(c, Some(rgb(247, 200, 94)));
    }

    #[test]
    fn ext_lookup_is_case_insensitive() {
        let c = resolve(&TreeLineType::File, "MAIN.RS", Some("RS"));
        assert_eq!(c, Some(rgb(255, 143, 64)));
    }

    #[test]
    fn dirname_lookup_folds_non_ascii_uppercase() {
        let c = resolve(&TreeLineType::Dir, "Público", None);
        assert_eq!(c, Some(rgb(91, 168, 255)));
        let c = resolve(&TreeLineType::Dir, "Música", None);
        assert_eq!(c, Some(rgb(215, 142, 255)));
    }

    #[test]
    fn symlink_target_root_falls_back_to_name() {
        // final_target="/" has no file_name(); target_name falls back
        // to the symlink's own name. "LICENSE" is in FILENAME_MAP, so
        // a hit there proves the fallback walked the name path.
        let c = resolve(
            &TreeLineType::SymLink {
                direct_target: "/".into(),
                final_is_dir: false,
                final_target: PathBuf::from("/"),
            },
            "LICENSE",
            None,
        );
        assert_eq!(c, Some(rgb(245, 216, 91)));
    }

}
