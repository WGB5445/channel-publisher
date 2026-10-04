# Security Policy

Do not post credentials, private channel data, or vulnerability details in a public issue. If you find a security problem, use GitHub's **Report a vulnerability** feature in this repository's Security tab when available. If that option is unavailable, open a minimal issue asking for a private contact channel without including exploit details.

Keep Bot Tokens and the administrator password outside version control. Run the dashboard behind an HTTPS reverse proxy when accessing it remotely, restrict who can reach it, and back up the SQLite database securely. Rotate any token you suspect has leaked.
