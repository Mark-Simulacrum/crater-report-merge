#[macro_use]
extern crate serde_derive;
extern crate serde;

#[derive(Serialize, Deserialize)]
pub struct TestResults {
    pub crates: Vec<CrateResult>,
}

#[derive(Serialize, Deserialize)]
pub struct CrateResult {
    pub name: String,
    pub url: String,
    pub res: Comparison,
    pub runs: [Option<BuildTestResult>; 2],
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    Regressed,
    Fixed,
    Skipped,
    Unknown,
    SameBuildFail,
    SameTestFail,
    SameTestSkipped,
    SameTestPass,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BuildTestResult {
    pub res: TestResult,
    pub log: String,
}

macro_rules! string_enum {
    (pub enum $name:ident { $($item:ident => $str:expr,)* }) => {
        #[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Copy, Clone)]
        pub enum $name {
            $($item,)*
        }

        impl ::std::str::FromStr for $name {
            type Err = ();

            fn from_str(s: &str) -> Result<$name, ()> {
                Ok(match s {
                    $($str => $name::$item,)*
                    s => panic!("invalid {}: {}", stringify!($name), s),
                })
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                write!(f, "{}", self.to_str())
            }
        }

        impl $name {
            pub fn to_str(&self) -> &'static str {
                match *self {
                    $($name::$item => $str,)*
                }
            }

            pub fn possible_values() -> &'static [&'static str] {
                &[$($str,)*]
            }
        }
    }
}

string_enum!(pub enum TestResult {
    BuildFail => "build-fail",
    TestFail => "test-fail",
    TestSkipped => "test-skipped",
    TestPass => "test-pass",
});
