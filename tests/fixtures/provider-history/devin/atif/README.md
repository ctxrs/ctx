# Devin CLI ATIF fixture

This sanitized fixture is hand-generated from the public ATIF root/step shape
and Devin CLI documentation that describes `devin --export [PATH]` as an ATIF
conversation export.

It is not copied from a Devin account, cloud session, local config directory, or
login state. It contains only synthetic prompts, responses, tool names, paths,
and token counts for importer tests.

It remains synthetic because the supported ctx contract is the official
user-supplied `devin --export [PATH]` ATIF JSON file or directory, and this
fixture set does not inspect Devin config/auth caches, authenticate to Devin, or
scrape cloud history.
