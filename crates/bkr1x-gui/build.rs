#[cfg(windows)]
#[path = "../win_exe_resources.rs"]
mod win_exe_resources;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    win_exe_resources::embed_exe_resources(
        "bkr1x-gui.ico",
        "Fire Phoenix BK-R1X Configurator",
        "Fire Phoenix BK-R1X Native PAW3311 Configurator",
        [106, 206, 158, 255],
        false,
    )?;
    Ok(())
}
