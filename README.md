# athena-connect

Gives your [Athena](https://github.com/OhPorty) access to **one folder** on this computer, for as long as the terminal stays open. Athena keeps running on its server; this small program is how it reaches the folder.

```
cd ~/projects/my-app
athena-connect
```

It prints a link and a QR code. Open it on your phone or any computer, and you're in a conversation where Athena works in `~/projects/my-app`. You can type here too. Close the terminal (or press Ctrl+C) and Athena's access ends.

## Install

**Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/OhPorty/athena-connect/main/install.sh | sh
```

**Windows** (PowerShell)

```powershell
irm https://raw.githubusercontent.com/OhPorty/athena-connect/main/install.ps1 | iex
```

Both download the latest release, check it against the release's `SHA256SUMS`, and install it for your user (no admin rights). You can also download a program from [Releases](https://github.com/OhPorty/athena-connect/releases) yourself, or build it with `cargo build --release`.

## Use

```
athena-connect login https://your-athena   # once per computer: your username, password and authenticator code
athena-connect [folder]                    # give Athena that folder (default: the one you're in)
athena-connect logout                      # forget this computer's key
```

Logging in gives this computer its own key, which is stored in your user's config folder and is readable only by you. Your password isn't stored. You can remove a computer at any time in Athena's settings under Account & security. Its key then stops working.

In the terminal you'll see what Athena reads, edits and runs. When it asks permission (for example, to edit a file or run a command that changes things), answer `y` or `n` here or in the website.

## Only that folder

Athena can't reach anything outside the folder you started `athena-connect` in. This is enforced here, on your computer, by this program and your operating system. It doesn't rely on Athena (or the model behind it) behaving.

- **Files.** Every path is resolved on this computer, including `..`, absolute paths and symbolic links. Anything that leads outside the folder is refused.
- **Commands (Linux).** Commands run in a sandbox ([Landlock](https://docs.kernel.org/userspace-api/landlock.html), Linux 6.2 or newer). They can read and write the folder and a private temporary folder. They can read, but not change, the system's programs and libraries and your installed toolchains (Rust, Node, Python). Nothing else is reachable: no home folder, no SSH keys, no other projects. If this computer can't sandbox commands, `athena-connect` turns them off rather than running them unconfined.
- **Commands (Windows).** Not available yet. On Windows, Athena can read, search and edit files in the folder.
- **When you close the terminal**, every command Athena started is stopped and the private temporary folder is removed.

## Releases

Pushing a tag like `v0.1.0` builds the Linux (x86_64 and ARM64) and Windows programs and publishes them, with checksums, as a GitHub release.
