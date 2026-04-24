use crate::registry::*;

pub fn defs() -> Vec<ServiceDef> {
    vec![ServiceDef {
        id: "embeddings",
        label: "Memory embeddings",
        category: Category::Memory,
        description: Some("API key opcional para embeddings hosted (cohere, openai-embed, etc.). Solo si `memory.yaml` apunta a un provider remoto."),
        fields: vec![FieldDef {
            key: "api_key",
            label: "Embeddings API key",
            help: Some("Déjalo vacío si usas embeddings locales."),
            kind: FieldKind::Secret,
            required: false,
            default: None,
            target: FieldTarget::Secret {
                file: "embedding_api_key.txt",
                env_var: "EMBEDDING_API_KEY",
            },
            validator: None,
        }],
    }]
}
