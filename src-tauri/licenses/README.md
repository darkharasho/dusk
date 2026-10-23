# Third-party programs shipped with Dusk

Dusk ships two programs that are not its own. Both are separate executables
that Dusk starts and talks to over sockets and command lines; neither is
linked into Dusk. Both are licensed under the GNU General Public License,
version 3, whose text is in `GPL-3.0.txt` beside this file.

| Program | What it does in Dusk | Source |
| --- | --- | --- |
| Dusk's Moonlight client, a fork of moonlight-qt | Shows the stream from another machine | <https://github.com/darkharasho/dusk-client> |
| Sunshine | Lets other machines stream from this one | <https://github.com/LizardByte/Sunshine> |

The exact versions in this copy of Dusk are listed in `versions.txt` beside
this file, as git tags in the repositories above. The complete corresponding
source for each is the source tree at that tag, and it stays available there
for as long as this version of Dusk is distributed. If it ever is not, ask
for it at <https://github.com/darkharasho/dusk/issues> and it will be
provided at no charge beyond the cost of sending it.

A build of Dusk with no `versions.txt` here was not built by its release
process and may not carry either program.
