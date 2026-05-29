use std::fmt;

#[derive(Debug, Eq, PartialEq)]
pub enum DiffTypes {
    Added,
    Deleted,
    Renamed,
}

#[derive(Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum DiffAction {
    ModifiedUser = 1,
    DeletedGroup = 2,
    DeletedUser = 3,
    AddedGroup = 4,
    AddedUser = 5,
}

impl fmt::Display for DiffTypes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DiffTypes::Added => "new file mode",
            DiffTypes::Deleted => "deleted file mode",
            DiffTypes::Renamed => "similarity index 100%",
        };
        write!(f, "{}", s)
    }
}
