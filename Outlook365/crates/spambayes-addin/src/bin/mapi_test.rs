//! Quick MAPI diagnostic tool — tests folder enumeration.
//!
//! Run with: cargo run --bin mapi_test --target x86_64-pc-windows-msvc

fn main() {
    // Initialize COM
    println!("=== SpamBayes MAPI Diagnostic ===");
    println!();
    println!("[1] Initializing COM...");
    let hr = unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    };
    println!("    CoInitializeEx result: {:?}", hr);

    println!("[2] Creating MAPI session...");
    let mut session = match spambayes_mapi::MapiSessionImpl::initialize_and_logon() {
        Ok(s) => {
            println!("    SUCCESS: MAPI session created.");
            s
        }
        Err(e) => {
            println!("    FAILED: {e}");
            println!();
            println!("    This usually means:");
            println!("    - Outlook is not installed or not configured");
            println!("    - No default MAPI profile exists");
            println!("    - COM was not properly initialized");
            return;
        }
    };

    println!("[3] Getting profile name...");
    match session.get_profile_name() {
        Ok(name) => println!("    Profile: {name}"),
        Err(e) => println!("    Could not get profile name: {e}"),
    }

    println!("[4] Enumerating message stores...");
    match session.enumerate_stores() {
        Ok(stores) => {
            println!("    Found {} store(s):", stores.len());
            for (i, store) in stores.iter().enumerate() {
                println!(
                    "      [{}] '{}' (default={}, eid_len={})",
                    i,
                    store.display_name,
                    store.is_default,
                    store.entry_id.len()
                );
            }

            // Try to open each store and get its folder hierarchy
            for store_info in &stores {
                println!();
                println!("[5] Opening store '{}'...", store_info.display_name);
                match session.open_store(&store_info.entry_id) {
                    Ok(store_ptr) => {
                        println!("    SUCCESS: store opened (ptr={:?})", store_ptr);
                        let store_ops = unsafe {
                            spambayes_mapi::MessageStoreOps::new(
                                store_ptr,
                                store_info.entry_id.clone(),
                            )
                        };

                        println!("[6] Getting root folder...");
                        match store_ops.get_root_folder() {
                            Ok(root) => {
                                println!(
                                    "    Root folder: '{}' (eid_len={}, count={})",
                                    root.name,
                                    root.entry_id.len(),
                                    root.count
                                );

                                println!("[7] Getting folder hierarchy...");
                                match store_ops.get_folder_hierarchy() {
                                    Ok(nodes) => {
                                        println!("    Found {} top-level folders:", nodes.len());
                                        for node in &nodes {
                                            println!("      - {} ({} children)", node.name, node.children.len());
                                        }
                                    }
                                    Err(e) => {
                                        println!("    FAILED to get hierarchy: {e}");
                                    }
                                }
                            }
                            Err(e) => {
                                println!("    FAILED to get root folder: {e}");
                            }
                        }
                    }
                    Err(e) => {
                        println!("    FAILED to open store: {e}");
                    }
                }
            }
        }
        Err(e) => {
            println!("    FAILED: {e}");
        }
    }

    println!();
    println!("=== Done ===");

    unsafe {
        windows::Win32::System::Com::CoUninitialize();
    }
}
