use std::collections::HashMap;
use std::error::Error;
use std::fs::read_to_string;
use std::sync::Arc;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use new_string_template::template::Template;

/// Represents a collection of templates loaded from a YAML file
#[derive(Debug, Serialize, Deserialize)]
pub struct Templates {
    #[serde(flatten)]
    templates: HashMap<String, String>,
}

impl Templates {
    /// Load templates from a YAML file
    pub fn from_yaml(yaml_content: &str) -> Result<Self, Box<dyn Error>> {
        let templates: Templates = serde_yaml::from_str(yaml_content)?;
        Ok(templates)
    }

    /// Get a template by name
    pub fn get(&self, name: &str) -> Option<&String> {
        self.templates.get(name)
    }

    /// Render a template with the given context
    pub fn render(&self, name: &str, context: &serde_json::Value) -> Result<String, Box<dyn Error>> {
        let template_str = self.get(name).ok_or_else(|| format!("Template '{}' not found", name))?;
        
        let template = Template::new(template_str);
        
        // Convert the serde_json::Value to a HashMap for the template
        let context_map: HashMap<String, String> = if let serde_json::Value::Object(map) = context {
            map.iter()
                .map(|(key, value)| {
                    let value_str = match value {
                        serde_json::Value::String(s) => s.clone(),
                        _ => value.to_string(),
                    };
                    (key.clone(), value_str)
                })
                .collect()
        } else {
            return Err("Context must be a JSON object".into());
        };
        
        // Convert to the format expected by new_string_template
        let context_refs: HashMap<&str, &str> = context_map
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        
        let rendered = template.render(&context_refs).map_err(|e| Box::new(e) as Box<dyn Error>)?;
        Ok(rendered)
    }
}

/// A singleton instance of the Templates struct.
/// This allows us to load the templates once and reuse them throughout the application.
pub static TEMPLATES: Lazy<Arc<Templates>> = Lazy::new(|| {
    let yaml = std::fs::read_to_string("src/utils/prompts/prompts.yaml")
        .expect("Failed to read templates.yaml file");
    
    let templates = Templates::from_yaml(&yaml)
        .expect("Failed to parse templates.yaml file");
    
    Arc::new(templates)
});

/// Get a template by name
pub fn get_template(name: &str) -> Option<&String> {
    TEMPLATES.get(name)
}

/// Render a template with the given context
pub fn render_template(name: &str, context: &serde_json::Value) -> Result<String, Box<dyn Error>> {
    TEMPLATES.render(name, context)
}
