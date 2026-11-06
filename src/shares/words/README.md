# Passphrase word lists

Share and public-upload passwords default to four random words joined by `-`
(`tanne-kiesel-ofen-raupe`), from the list of the creator's UI language.

- `en.txt`: the EFF Short Wordlist #1 by the Electronic Frontier Foundation,
  <https://www.eff.org/dice>, licensed under the Creative Commons Attribution 3.0 License
  (<https://creativecommons.org/licenses/by/3.0/us/>). Kept unchanged; chestcraft skips the
  one entry with a hyphen (`yo-yo`), since `-` separates the words.
- `de.txt`: written for chestcraft, under the repository's license (MIT OR Apache-2.0).

More languages: `[shares.wordlists]` in the config maps a language code to a file with one
word per line (the last field of a line counts, so dice-numbered lists work as they are;
`#` starts a comment).
