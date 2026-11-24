![BTWN for Windows — sync local Codex chats between a desktop and a laptop](assets/btwn-cover.svg)

# BTWN

Pick up a Codex conversation on your laptop where you left it on your desktop. BTWN transfers saved chats, their project sidebar, and optional project files directly between two Windows PCs on the same private network. After a completed transfer, you can switch the sending PC off and open the local copy on the other computer.

**[Download the Windows x64 installer](https://github.com/iaMO3/BTWN/releases/latest)** · [First-time setup](#first-time-setup) · [Troubleshooting](#troubleshooting)

## First-time setup

1. **Install on both PCs.** Run the Windows x64 MSI on each. Open the Codex desktop app at least once on both computers and have a saved local chat on each. Use the same up-to-date Codex version for your first transfer.
2. **Pair once.** Open BTWN on the same private Wi-Fi or wired LAN. On one PC, click **Create pairing file**. Send that JSON privately to your other PC, then click **Open pairing file** there. Anyone with the file has the credential for that pair. The PC that creates it hosts the connection.
3. **Choose what to send.** Fully quit Codex or ChatGPT on both PCs, including the tray. Leave BTWN open on both. On the PC with the history you want, select **Send**. Turn on **Include project files** if you also want the files behind those chats.
4. **Click Start sync.** Both windows show transfer progress. When the receiver confirms completion, reopen Codex there and check the latest message in a familiar chat. If BTWN flags items for review, open **Settings → Transfers → Open received files**.

## Pick a direction

| From the PC in front of you | What happens |
| --- | --- |
| **Send** | Makes the sender's saved conversation the active version of a shared chat on the other PC when it can do so safely. |
| **Receive** | Pulls the other PC's saved chats onto this one. |
| **Automatic** | Checks both ways about every two minutes while BTWN is open. Divergent histories are kept for review. |

Use **Pause sync** on either computer to pause the pair. Resume from either window.

### Coming from a private preview?

Version 1.1.0 is the first public pairing format. Upgrade both PCs together and create a fresh pairing file. The public version stores its connection and transfer data under `%LOCALAPPDATA%\BTWN`. It leaves earlier BTWN storage in place.

## What goes with you

| Item | How BTWN handles it |
| --- | --- |
| **Local Codex chats** | Transfers saved conversations and available attachments. Before replacing a differing conversation in one-way mode, BTWN verifies a recoverable copy on the receiver. Dependent histories can require review. |
| **Project sidebar** | Restores project names, icons, order, groups, and expansion state. Receiver-only projects and chats are kept. |
| **Project files** | Compares files first, skips identical content, and keeps conflicting file versions separately. Choose the folder for new projects in **Settings → Transfers**. Generated folders such as `node_modules`, `target`, and build caches are skipped. |

BTWN works with the Windows Codex desktop app’s local storage. Regular ChatGPT website conversations and cloud workspace memberships are outside its scope. Old messages can contain absolute file links that still refer to the original PC. BTWN is an independent app and is unaffiliated with OpenAI.

## Troubleshooting

<details>
<summary><strong>The other computer stays on “Searching”</strong></summary>

Keep BTWN open on both computers and use the same public release on each. Check that both are on the same trusted LAN. Guest Wi-Fi can isolate devices. On the network you trust, use the Windows **Private** network profile. The installer registers local-subnet rules for TCP and UDP port **53328**. If you need another copy of the invitation, use **Settings → Transfers → Export pairing file** on the hosting PC and share it privately.

</details>

<details>
<summary><strong>The receiver still shows an earlier message</strong></summary>

Finish the conversation and fully quit Codex on both computers so the latest turn is saved. Use **Send** on the computer containing that turn. Wait for the receiving computer’s confirmation and read any conflict report before reopening Codex. For a first transfer, test a short chat with an unmistakable final message.

</details>

<details>
<summary><strong>What should I include in a bug report?</strong></summary>

Include the BTWN and Codex versions on both PCs, the sync direction, and the exact status text. Redact device names and personal paths. Keep pairing files, private keys, chat databases, and conversation text out of public issues. [Report a problem](https://github.com/iaMO3/BTWN/issues)

</details>

## License

BTWN is free to install and use. The author retains all other rights. See the [software license](LICENSE.txt). Third-party components retain their own licenses, included with the installer.
