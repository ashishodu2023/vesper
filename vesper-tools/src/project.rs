use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub languages: Vec<String>,
    pub project_type: String,
    pub key_files: Vec<String>,
    pub suggested_verify: Option<String>,
    pub summary: String,
}

pub fn scan_project(root: &Path) -> ProjectInfo {
    let mut languages = Vec::new();
    let mut key_files = Vec::new();
    let mut project_type = "unknown".to_string();
    let mut suggested_verify = None;

    let checks: &[(&str, &str, Option<&str>)] = &[
        ("Cargo.toml", "Rust", Some("cargo test")),
        ("package.json", "Node/JS", Some("npm test")),
        ("pyproject.toml", "Python", Some("pytest -q")),
        ("requirements.txt", "Python", Some("pytest -q")),
        ("go.mod", "Go", Some("go test ./...")),
        ("pom.xml", "Java/Maven", Some("mvn test")),
        ("build.gradle", "Java/Gradle", Some("./gradlew test")),
        ("CMakeLists.txt", "C/C++", None),
        ("Makefile", "Make", None),
    ];

    for (file, lang, verify) in checks {
        if root.join(file).exists() {
            key_files.push((*file).into());
            if !languages.iter().any(|l| l == lang) {
                languages.push((*lang).into());
            }
            if project_type == "unknown" {
                project_type = lang.to_lowercase();
            }
            if suggested_verify.is_none() {
                if let Some(v) = verify {
                    suggested_verify = Some((*v).into());
                }
            }
        }
    }

    // Light file extension sample at top level
    if let Ok(rd) = std::fs::read_dir(root) {
        for ent in rd.flatten().take(80) {
            let name = ent.file_name().to_string_lossy().into_owned();
            if let Some(ext) = name.rsplit_once('.').map(|(_, e)| e) {
                let lang = match ext {
                    "rs" => Some("Rust"),
                    "py" => Some("Python"),
                    "ts" | "tsx" | "js" | "jsx" => Some("TypeScript/JS"),
                    "go" => Some("Go"),
                    "java" => Some("Java"),
                    "cu" | "cuh" => Some("CUDA"),
                    "cpp" | "cc" | "hpp" | "c" | "h" => Some("C/C++"),
                    _ => None,
                };
                if let Some(l) = lang {
                    if !languages.iter().any(|x| x == l) {
                        languages.push(l.into());
                    }
                }
            }
        }
    }

    if languages.is_empty() {
        languages.push("unknown".into());
    }

    let summary = format!(
        "type={project_type}; languages={}; key_files={}",
        languages.join(","),
        if key_files.is_empty() {
            "(none)".into()
        } else {
            key_files.join(",")
        }
    );

    ProjectInfo {
        languages,
        project_type,
        key_files,
        suggested_verify,
        summary,
    }
}
