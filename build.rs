use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=JADE_RT_SRC_DIR");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out_file = out_dir.join("jade_rt_sources.rs");

    if let Ok(src_dir) = env::var("JADE_RT_SRC_DIR") {
        let src = PathBuf::from(&src_dir);

        let h_dst  = out_dir.join("jade_rt.h");
        let pt_dst = out_dir.join("jade_rt_pthread.c");
        let jo_dst = out_dir.join("jade_rt_jadeos.c");

        fs::copy(src.join("jade_rt.h"),         &h_dst)
            .expect("JADE_RT_SRC_DIR set but jade_rt.h not found");
        fs::copy(src.join("jade_rt_pthread.c"), &pt_dst)
            .expect("JADE_RT_SRC_DIR set but jade_rt_pthread.c not found");
        fs::copy(src.join("jade_rt_jadeos.c"),  &jo_dst)
            .expect("JADE_RT_SRC_DIR set but jade_rt_jadeos.c not found");

        println!("cargo:rerun-if-changed={}/jade_rt.h", src_dir);
        println!("cargo:rerun-if-changed={}/jade_rt_pthread.c", src_dir);
        println!("cargo:rerun-if-changed={}/jade_rt_jadeos.c", src_dir);

        let h  = h_dst.to_str().unwrap().replace('\\', "\\\\");
        let pt = pt_dst.to_str().unwrap().replace('\\', "\\\\");
        let jo = jo_dst.to_str().unwrap().replace('\\', "\\\\");

        fs::write(&out_file, format!(
            "pub const RT_HEADER:  Option<&'static str> = Some(include_str!(\"{h}\"));\n\
             pub const RT_PTHREAD: Option<&'static str> = Some(include_str!(\"{pt}\"));\n\
             pub const RT_JADEOS:  Option<&'static str> = Some(include_str!(\"{jo}\"));\n",
        )).unwrap();
    } else {
        fs::write(&out_file,
            "pub const RT_HEADER:  Option<&'static str> = None;\n\
             pub const RT_PTHREAD: Option<&'static str> = None;\n\
             pub const RT_JADEOS:  Option<&'static str> = None;\n",
        ).unwrap();
    }
}
