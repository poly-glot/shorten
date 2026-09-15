use std::collections::HashSet;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const APP_MODULE: &str = "app.js";
const DECLARATION_KEYWORDS: [&str; 5] = ["async function ", "class ", "const ", "function ", "let "];
const IMPORT_FROM: &str = " from \"";
const IMPORT_LIST_OPEN: char = '{';
const IMPORT_TERMINATOR: &str = "\";";
const MODULE_SCRIPT_TAG: &str = "<script type=\"module\" src=\"app.js\"></script>";
const PAGE: &str = "index.html";
const RELATIVE_PREFIX: &str = "./";
const STYLESHEET_TAG_CLOSE: &str = "\">";
const STYLESHEET_TAG_OPEN: &str = "<link rel=\"stylesheet\" href=\"";

#[derive(Debug)]
pub enum BundleError {
    Cycle(PathBuf),
    DuplicateName { module: PathBuf, name: String },
    Read { error: io::Error, path: PathBuf },
    UnsupportedSyntax { line: String, module: PathBuf },
}

impl Display for BundleError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cycle(module) => write!(f, "{} imports itself through its dependencies", module.display()),
            Self::DuplicateName { module, name } => write!(
                f,
                "{} declares `{name}`, which another module already declares; the inlined page shares one scope",
                module.display()
            ),
            Self::Read { error, path } => write!(f, "could not read {}: {error}", path.display()),
            Self::UnsupportedSyntax { line, module } => {
                write!(f, "{} has a line the bundler cannot inline: {line}", module.display())
            }
        }
    }
}

struct Module {
    body: String,
    imports: Vec<PathBuf>,
    names: Vec<String>,
}

#[derive(Default)]
struct Bundler {
    bodies: Vec<String>,
    emitted: HashSet<PathBuf>,
    names: HashSet<String>,
    visiting: Vec<PathBuf>,
}

pub fn bundle(frontend: &Path) -> Result<String, BundleError> {
    let page = read(&frontend.join(PAGE))?;
    let page = inline_stylesheets(page, frontend)?;

    inline_script(page, frontend)
}

fn read(path: &Path) -> Result<String, BundleError> {
    fs::read_to_string(path).map_err(|error| BundleError::Read {
        error,
        path: path.to_path_buf(),
    })
}

fn unsupported(module: &Path, line: &str) -> BundleError {
    BundleError::UnsupportedSyntax {
        line: line.to_string(),
        module: module.to_path_buf(),
    }
}

fn inline_stylesheets(mut page: String, frontend: &Path) -> Result<String, BundleError> {
    while let Some(start) = page.find(STYLESHEET_TAG_OPEN) {
        let href_start = start + STYLESHEET_TAG_OPEN.len();
        let Some(href_len) = page[href_start..].find(STYLESHEET_TAG_CLOSE) else {
            return Err(unsupported(Path::new(PAGE), &page[start..]));
        };

        let href = page[href_start..href_start + href_len].to_string();
        let stylesheet = read(&frontend.join(&href))?;
        let end = href_start + href_len + STYLESHEET_TAG_CLOSE.len();

        page.replace_range(start..end, &format!("<style>\n{stylesheet}</style>"));
    }

    Ok(page)
}

fn inline_script(page: String, frontend: &Path) -> Result<String, BundleError> {
    if !page.contains(MODULE_SCRIPT_TAG) {
        return Err(unsupported(Path::new(PAGE), MODULE_SCRIPT_TAG));
    }

    let mut bundler = Bundler::default();
    bundler.include(&frontend.join(APP_MODULE))?;

    let script = bundler.bodies.join("\n\n");

    Ok(page.replacen(MODULE_SCRIPT_TAG, &format!("<script type=\"module\">\n{script}\n</script>"), 1))
}

impl Bundler {
    fn include(&mut self, path: &Path) -> Result<(), BundleError> {
        let path = fs::canonicalize(path).map_err(|error| BundleError::Read {
            error,
            path: path.to_path_buf(),
        })?;

        if self.emitted.contains(&path) {
            return Ok(());
        }
        if self.visiting.contains(&path) {
            return Err(BundleError::Cycle(path));
        }

        let module = parse(&path, &read(&path)?)?;

        self.visiting.push(path.clone());
        for import in &module.imports {
            self.include(import)?;
        }
        self.visiting.pop();

        for name in module.names {
            if !self.names.insert(name.clone()) {
                return Err(BundleError::DuplicateName { module: path, name });
            }
        }

        self.emitted.insert(path);
        self.bodies.push(module.body);

        Ok(())
    }
}

fn parse(path: &Path, source: &str) -> Result<Module, BundleError> {
    let Some(dir) = path.parent() else {
        return Err(unsupported(path, APP_MODULE));
    };

    let mut body = Vec::new();
    let mut imports = Vec::new();
    let mut names = Vec::new();

    for line in source.lines() {
        if let Some(clause) = line.strip_prefix("import ") {
            let Some(specifier) = import_specifier(clause) else {
                return Err(unsupported(path, line));
            };

            imports.push(dir.join(specifier));
            continue;
        }

        if line.contains("import(") || line.contains("import.meta") {
            return Err(unsupported(path, line));
        }

        let declaration = match line.strip_prefix("export ") {
            Some(exported) if exported.starts_with("default ") || exported.starts_with(['{', '*']) => {
                return Err(unsupported(path, line));
            }
            Some(exported) => exported,
            None => line,
        };

        if let Some(name) = declared_name(declaration) {
            names.push(name);
        }

        body.push(declaration);
    }

    Ok(Module {
        body: body.join("\n").trim().to_string(),
        imports,
        names,
    })
}

fn import_specifier(clause: &str) -> Option<&str> {
    if !clause.starts_with(IMPORT_LIST_OPEN) {
        return None;
    }

    let (_, tail) = clause.split_once(IMPORT_FROM)?;
    let (specifier, _) = tail.split_once(IMPORT_TERMINATOR)?;

    specifier.starts_with(RELATIVE_PREFIX).then_some(specifier)
}

fn declared_name(declaration: &str) -> Option<String> {
    let rest = DECLARATION_KEYWORDS.iter().find_map(|keyword| declaration.strip_prefix(keyword))?;
    let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$')).collect();

    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::process;

    use super::*;

    const PAGE_WITH_ASSETS: &str = "<head>\n<link rel=\"stylesheet\" href=\"styles/01-a.css\">\n<link rel=\"stylesheet\" href=\"styles/02-b.css\">\n<script type=\"module\" src=\"app.js\"></script>\n</head>\n";

    fn fixture(label: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = env::temp_dir().join(format!("shorten-bundle-{}-{label}", process::id()));

        for (name, content) in files {
            let path = dir.join(name);
            let Some(parent) = path.parent() else {
                panic!("{label}: {name} has no parent directory");
            };

            if let Err(error) = fs::create_dir_all(parent).and_then(|()| fs::write(&path, content)) {
                panic!("{label}: could not write {name}: {error}");
            }
        }

        dir
    }

    fn built(label: &str, files: &[(&str, &str)]) -> String {
        let dir = fixture(label, files);
        let page = bundle(&dir);
        let _ = fs::remove_dir_all(&dir);

        match page {
            Ok(page) => page,
            Err(error) => panic!("{label}: {error}"),
        }
    }

    fn refused(label: &str, files: &[(&str, &str)]) -> BundleError {
        let dir = fixture(label, files);
        let page = bundle(&dir);
        let _ = fs::remove_dir_all(&dir);

        match page {
            Ok(_) => panic!("{label}: bundled a page it should have refused"),
            Err(error) => error,
        }
    }

    fn position(page: &str, needle: &str) -> usize {
        match page.find(needle) {
            Some(position) => position,
            None => panic!("`{needle}` is missing from the page"),
        }
    }

    #[test]
    fn inlines_stylesheets_in_link_order() {
        let page = built(
            "stylesheets",
            &[
                ("index.html", PAGE_WITH_ASSETS),
                ("styles/01-a.css", "a { color: red; }\n"),
                ("styles/02-b.css", "b { color: blue; }\n"),
                ("app.js", "const app = 1;\n"),
            ],
        );

        assert_eq!(page.matches("<link").count(), 0, "every link tag must be replaced");
        assert!(
            position(&page, "a { color: red; }") < position(&page, "b { color: blue; }"),
            "the cascade order must survive"
        );
    }

    #[test]
    fn inlines_modules_dependency_first_with_imports_and_exports_stripped() {
        let page = built(
            "modules",
            &[
                ("index.html", PAGE_WITH_ASSETS),
                ("styles/01-a.css", ""),
                ("styles/02-b.css", ""),
                ("app.js", "import { wire } from \"./js/wire.js\";\n\nwire();\n"),
                (
                    "js/wire.js",
                    "import { el } from \"./dom.js\";\n\nexport function wire() {\n    el(\"x\");\n}\n",
                ),
                ("js/dom.js", "export const el = (id) => document.getElementById(id);\n"),
            ],
        );

        let script = &page[position(&page, "<script type=\"module\">")..];

        assert_eq!(script.matches("import ").count(), 0, "no import may survive inlining");
        assert_eq!(script.matches("export ").count(), 0, "no export may survive inlining");
        assert!(
            position(script, "const el =") < position(script, "function wire()"),
            "a module comes after what it imports"
        );
        assert!(position(script, "function wire()") < position(script, "wire();"), "app.js comes last");
    }

    #[test]
    fn emits_a_module_imported_twice_once() {
        let page = built(
            "shared",
            &[
                ("index.html", PAGE_WITH_ASSETS),
                ("styles/01-a.css", ""),
                ("styles/02-b.css", ""),
                ("app.js", "import { a } from \"./js/a.js\";\nimport { b } from \"./js/b.js\";\n\na();\nb();\n"),
                ("js/a.js", "import { el } from \"./dom.js\";\n\nexport const a = () => el(\"a\");\n"),
                ("js/b.js", "import { el } from \"./dom.js\";\n\nexport const b = () => el(\"b\");\n"),
                ("js/dom.js", "export const el = (id) => document.getElementById(id);\n"),
            ],
        );

        assert_eq!(page.matches("const el =").count(), 1);
    }

    #[test]
    fn refuses_what_a_shared_scope_cannot_express() {
        let cases = [
            (
                "a default export",
                vec![
                    ("index.html", PAGE_WITH_ASSETS),
                    ("styles/01-a.css", ""),
                    ("styles/02-b.css", ""),
                    ("app.js", "export default 1;\n"),
                ],
                "UnsupportedSyntax",
            ),
            (
                "a side-effect import",
                vec![
                    ("index.html", PAGE_WITH_ASSETS),
                    ("styles/01-a.css", ""),
                    ("styles/02-b.css", ""),
                    ("app.js", "import \"./js/x.js\";\n"),
                    ("js/x.js", ""),
                ],
                "UnsupportedSyntax",
            ),
            (
                "a dynamic import",
                vec![
                    ("index.html", PAGE_WITH_ASSETS),
                    ("styles/01-a.css", ""),
                    ("styles/02-b.css", ""),
                    ("app.js", "const x = await import(\"./js/x.js\");\n"),
                ],
                "UnsupportedSyntax",
            ),
            (
                "a top-level name declared in two modules",
                vec![
                    ("index.html", PAGE_WITH_ASSETS),
                    ("styles/01-a.css", ""),
                    ("styles/02-b.css", ""),
                    ("app.js", "import { a } from \"./js/a.js\";\n\nconst el = 1;\na(el);\n"),
                    ("js/a.js", "const el = 2;\n\nexport const a = (x) => x + el;\n"),
                ],
                "DuplicateName",
            ),
            (
                "an import cycle",
                vec![
                    ("index.html", PAGE_WITH_ASSETS),
                    ("styles/01-a.css", ""),
                    ("styles/02-b.css", ""),
                    ("app.js", "import { a } from \"./js/a.js\";\n\na();\n"),
                    ("js/a.js", "import { b } from \"./b.js\";\n\nexport const a = () => b();\n"),
                    ("js/b.js", "import { a } from \"./a.js\";\n\nexport const b = () => a();\n"),
                ],
                "Cycle",
            ),
            (
                "a module that does not exist",
                vec![
                    ("index.html", PAGE_WITH_ASSETS),
                    ("styles/01-a.css", ""),
                    ("styles/02-b.css", ""),
                    ("app.js", "import { a } from \"./js/missing.js\";\n"),
                ],
                "Read",
            ),
            (
                "a page that does not load app.js as a module",
                vec![("index.html", "<head></head>\n"), ("app.js", "")],
                "UnsupportedSyntax",
            ),
        ];

        for (label, files, expected) in cases {
            let error = refused(label, &files);
            let variant = match error {
                BundleError::Cycle(_) => "Cycle",
                BundleError::DuplicateName { .. } => "DuplicateName",
                BundleError::Read { .. } => "Read",
                BundleError::UnsupportedSyntax { .. } => "UnsupportedSyntax",
            };

            assert_eq!(variant, expected, "{label}");
        }
    }
}
