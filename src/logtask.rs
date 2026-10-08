#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Success,
}

/// A task that has been logged, which can then be updated to display success
#[derive(Clone, Debug)]
pub struct LogTask {
    pub message: String,
    leading_str: Option<String>,
    status: Status,
}

impl LogTask {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_owned(),
            leading_str: None,
            status: Status::Pending,
        }
    }

    pub fn get_message(&self) -> String {
        let mut r = String::new();
        if let Some(leading_str) = &self.leading_str {
            r += &format!("[{leading_str}] ");
        }
        r += &self.message;
        match self.status {
            Status::Pending => {}
            Status::Success => r += " done!",
        }
        r
    }

    pub fn set_leading_str(&mut self, leading_str: &str) {
        self.leading_str = Some(leading_str.to_owned());
    }

    pub fn set_status(&mut self, status: Status) {
        self.status = status;
    }
}
