/// Simple test to verify STEP file parsing without GUI.
use monstertruck::step::load::ruststep::parser::parse;
use std::{cmp::Reverse, collections::HashMap, fs, path::PathBuf, process};

fn main() {
    env_logger::init();

    let file_path = PathBuf::from("examples/abstract_pca.step");

    println!("Testing STEP parser with: {:?}", file_path);
    println!("═══════════════════════════════════════");

    // Read the STEP file.
    let file_content = fs::read_to_string(&file_path).unwrap_or_else(|e| {
        eprintln!("✗ Failed to read file: {}", e);
        process::exit(1);
    });
    println!("✓ File read successfully ({} bytes)", file_content.len());

    // Parse using ruststep.
    let exchange = parse(&file_content).unwrap_or_else(|e| {
        eprintln!("✗ Failed to parse STEP file: {:?}", e);
        process::exit(1);
    });
    println!("✓ STEP file parsed successfully");

    // Extract metadata.
    println!("\nHeader Information:");
    println!("───────────────────────────────────────");

    for record in &exchange.header {
        println!("  {}: {:?}", record.name, record.parameter);
    }

    // Count entities.
    let entity_count: usize = exchange
        .data
        .iter()
        .map(|section| section.entities.len())
        .sum();

    println!("\nEntity Statistics:");
    println!("───────────────────────────────────────");
    println!("  Total entities: {}", entity_count);
    println!("  Data sections: {}", exchange.data.len());

    // Sample first few entities.
    println!("\nSample Entities (first 10):");
    println!("───────────────────────────────────────");

    for (idx, entity) in exchange
        .data
        .iter()
        .flat_map(|section| section.entities.iter())
        .take(10)
        .enumerate()
    {
        let entity_str = format!("{:?}", entity);
        let entity_type = entity_str.split('(').next().unwrap_or("Unknown");
        let display = if entity_str.len() > 80 {
            format!("{}...", &entity_str[..77])
        } else {
            entity_str.clone()
        };
        println!("  #{}: {} - {}", idx, entity_type, display);
    }

    // Count entity types.
    println!("\nEntity Type Distribution:");
    println!("───────────────────────────────────────");

    let mut type_counts = HashMap::new();

    for entity in exchange
        .data
        .iter()
        .flat_map(|section| section.entities.iter())
    {
        let entity_str = format!("{:?}", entity);
        let entity_type = entity_str
            .split('(')
            .next()
            .unwrap_or("Unknown")
            .to_string();
        *type_counts.entry(entity_type).or_insert(0) += 1;
    }

    let mut sorted_types: Vec<_> = type_counts.iter().collect();
    sorted_types.sort_by_key(|&(_, count)| Reverse(*count));

    for (entity_type, count) in sorted_types.iter().take(15) {
        println!("  {:<30} {}", entity_type, count);
    }

    println!("\n✓ Parsing test completed successfully!");
}
