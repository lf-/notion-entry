# notion-entry

**NOTE**: This project is a work in progress and has a pile of FIXMEs

This program lets you input data into Notion from the command line. It exists
to avoid the inconsistent keyboard usage experience of the Notion database web
interface.

Currently it supports these column types:

* Text
* Title
* Select
* Date
* URL
* Relation (limited support: prefetches the relation data and doesn't do
  pagination for that yet)

## Usage

Install nightly Rust. See <https://rustup.rs>.

Checkout this repository. You can run the software directly from this checkout
by using `cargo run` in place of `notion-entry` in the commands that follow.
You can also install the software into your home directory with
`cargo install --locked --path .`.

Create a Notion integration: <https://www.notion.so/my-integrations>.

Share the database you want to enter data into and also any databases it has
relations to with the integration on the Notion web interface.

Run `notion-entry list`. It will tell you to create a file with your Notion app
token in it (on my machine, it is at `~/.config/notion-entry/token`). Create
that file.

Pick the database you want to enter items into from that list, and enter its
UUID in a file called `database` in the configuration directory you saw
previously.

Run `notion-entry add`. It will prompt you for the data you want to enter.

Notes on the UI:

* You can reorder fields by typing the `:order` command into any textual
  field.
* The relation selection uses the multi selection mode of
  [skim](https://github.com/lotabout/skim), which uses the <kbd>Tab</kbd> key
  to select items, and <kbd>Enter</kbd> to accept the input.
* Colours on select items are just what you have set on the Notion web
  interface.

