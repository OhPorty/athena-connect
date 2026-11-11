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
- **Commands (Linux).** Commands run in a sandbox ([Landlock](https://docs.kernel.org/userspace-api/landlock.html), Linux 6.2 or newer). They can read and write the folder and a private temporary folder. They can read, but not change, the system's programs and libraries and your toolchains' program folders (Rust, Node, Python). Secrets kept next to them, like Cargo's crates.io token, are not readable. Nothing else is reachable: no home folder, no SSH keys, no other projects.
- **Commands (Windows).** Commands run in PowerShell inside an [AppContainer](https://learn.microsoft.com/windows/win32/secauthz/appcontainer-isolation), the sandbox Windows uses for Store apps and browser tabs, with the same rules. A command in it can only open what has been opened to it: the folder, a private temporary folder, and (read only) the toolchain folders under your user folder that are on your PATH. Windows' own folders are readable as usual. Your documents, SSH keys and other projects are not. Some notes:
  - The first time you use a folder, `athena-connect` adds itself to the permissions of that folder and of your toolchain folders. For big folders this can take a minute; after that it's instant. These entries only let that folder's own sandbox in, and they stay so the setup isn't repeated.
  - The folder has to be on an NTFS drive. On FAT or exFAT drives (many USB sticks) and network drives, Windows has no permissions to enforce, so commands stay off there. For the same reason, a command could read a FAT or exFAT USB stick plugged into the computer.
  - Commands can download from the internet (cargo, npm, pip) but can't reach other computers on your local network.
  - During a session the folder also gets a free drive letter (like `subst`, e.g. `Z:`), which you may see in File Explorer. Commands start there, because PowerShell otherwise looks at the folders above where it starts. The drive letter is removed when the session ends.
- **Proven before use.** Before turning commands on, `athena-connect` runs a test command in the sandbox that tries to read a file outside the folder. Commands are turned on only if that command runs and the read fails. If this computer can't confine commands, they stay off and Athena can still read, search and edit files.
- **When you close the terminal**, every command Athena started is stopped and the private temporary folder is removed.

## Releases

Pushing a tag like `v0.1.0` builds the Linux (x86_64 and ARM64) and Windows programs and publishes them, with checksums, as a GitHub release.
