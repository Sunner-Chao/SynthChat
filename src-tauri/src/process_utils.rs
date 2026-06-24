pub(crate) trait CommandWindowExt {
    fn hide_window(&mut self) -> &mut Self;
}

impl CommandWindowExt for std::process::Command {
    fn hide_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

impl CommandWindowExt for tokio::process::Command {
    fn hide_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}
