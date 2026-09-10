pub struct Config {
    pub name: String,
}

pub enum Mode {
    Fast,
    Slow,
}

pub trait Runner {
    fn run(&self) -> u32;
}

impl Runner for Config {
    fn run(&self) -> u32 {
        1
    }
}
