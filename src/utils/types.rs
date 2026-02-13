use std::fmt;

#[derive(Debug, Eq, PartialEq)]
pub enum DiffTypes {
    Added,
    Deleted,
    Renamed,
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub enum DiffAction {
    AddedGroup,
    AddedUser,
    DeletedGroup,
    DeletedUser,
    ModifiedUser,
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
