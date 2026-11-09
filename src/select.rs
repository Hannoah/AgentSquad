//! Text selection in a session's terminal pane. Cells are (row, column)
//! inside the pane, so the geometry tests without a terminal.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Project index of the session the cells belong to.
    pub pi: usize,
    anchor: (u16, u16),
    head: (u16, u16),
    /// False until the pointer leaves the first cell: a plain click selects nothing.
    pub moved: bool,
}
impl Selection {
    pub fn new(pi: usize, at: (u16, u16)) -> Self {
        Self {
            pi,
            anchor: at,
            head: at,
            moved: false,
        }
    }
    pub fn drag_to(&mut self, at: (u16, u16)) {
        self.head = at;
        self.moved |= at != self.anchor;
    }
    /// First and last selected cell in reading order, both included.
    pub fn span(&self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
    /// Reading-order selection, like a terminal's: the first row from its
    /// start cell, whole rows between, the last row up to its end cell.
    pub fn contains(&self, row: u16, col: u16) -> bool {
        let (first, last) = self.span();
        (first..=last).contains(&(row, col))
    }
}
