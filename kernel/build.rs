fn main() {
    println!("cargo:rerun-if-changed=../../userspace/agent_visual_term/src/main.rs");
    println!("cargo:rerun-if-changed=../../userspace/agent_visual_term/target/x86_64-aetherion-user/release/agent_visual_term");
}
