//! Replay the current structured recipe directly through the renderer.
use atelier_studio::{Studio, source::Source};
use std::path::Path;
pub async fn run(args: &[String]) -> i32 {
    let mut path = None;
    let mut home = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--help" | "-h" => {
                println!("usage: atelier replay <recipe.atelier|bundle|doc-id> [--home DIR]");
                return 0;
            }
            "--home" if home.is_none() => {
                index += 1;
                home = args.get(index).map(String::as_str);
                if home.is_none_or(|v| v.starts_with('-')) {
                    eprintln!("--home needs a directory");
                    return 2;
                }
            }
            value if value.starts_with('-') || path.is_some() => {
                eprintln!("unexpected replay argument: {value}");
                return 2;
            }
            value => path = Some(value),
        }
        index += 1;
    }
    let Some(input) = path else {
        eprintln!("replay needs a recipe or document id");
        return 2;
    };
    let studio = home.map_or_else(Studio::new, |h| Studio::with_home(h.into()));
    let file = Path::new(input);
    if file.extension().is_some_and(|e| e == "jsonl") {
        eprintln!("legacy JSONL: convert with atelier migrate <old-recipe> <new-directory>");
        return 2;
    }
    let resolved = if file.exists() {
        file.to_path_buf()
    } else if input.parse::<atelier_studio::DocumentId>().is_ok() {
        studio.recipe_path(input).expect("validated document id")
    } else {
        eprintln!("no recipe or document '{input}'");
        return 2;
    };
    match Source::load(&resolved).and_then(|source| studio.build_source(&source)) {
        Ok(result) => {
            println!("{result}");
            0
        }
        Err(error) => {
            eprintln!("replay: {error}");
            1
        }
    }
}
