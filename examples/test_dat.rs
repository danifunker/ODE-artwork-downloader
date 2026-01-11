use ode_artwork_downloader::api::RedumpDatabase;

fn main() {
    env_logger::init();

    let mut db = RedumpDatabase::new();

    // Try loading the ZIP file
    let zip_path = std::env::var("HOME").unwrap() + "/Downloads/IBM - PC compatible - Datfile (56405) (2026-01-11 03-15-56).zip";

    println!("Loading DAT from: {}", zip_path);

    match db.load_dat(&zip_path) {
        Ok(count) => {
            println!("Loaded {} games", count);
            println!("System: {:?}", db.system_name);

            // Test finding by filename
            let matches = db.find_by_filename("Batman Forever (Europe) (Track 01).bin");
            println!("\nFilename matches for 'Batman Forever (Europe) (Track 01).bin':");
            for game in matches {
                println!("  - {}", game.name);
            }

            // Test fuzzy search
            let results = db.find_by_name_fuzzy("batman forever", 5);
            println!("\nFuzzy matches for 'batman forever':");
            for (game, score) in results {
                println!("  - {} (score: {:.2})", game.name, score);
            }

            // Test with volume label
            let results = db.find_by_name_fuzzy("QUAKE", 5);
            println!("\nFuzzy matches for 'QUAKE':");
            for (game, score) in results {
                println!("  - {} (score: {:.2})", game.name, score);
            }
        }
        Err(e) => {
            eprintln!("Failed to load DAT: {}", e);
        }
    }
}
