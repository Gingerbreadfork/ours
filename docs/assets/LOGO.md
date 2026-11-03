# The Ours logo

Ours is French for bear, and the mark is the group's bear: round and warm,
its head tilted to listen, with no mouth because it listens far more than it
speaks. In the lockup its head is the height of a capital and it stands on
the baseline with the letters, one more member of the row.

## Colours

| Name | Hex | For |
|---|---|---|
| Clay | `#c8562c` | The bear. The colour people should remember; elsewhere use it for one accent at a time. |
| Cream | `#f8e7d2` | The muzzle, the ground of the avatar, and the word on dark pages. |
| Ink | `#2b1a14` | The eyes and nose, the word on light pages, and the one-colour mark. |

Clay has 4.35:1 contrast on white and on GitHub's dark `#0d1117`. On
Discord's mid-grey `#313338` it drops to 2.9:1, so use the avatar there.

## Typeface

The word is Noto Serif Bold, converted to outlines, so no font is needed to
show it. The Noto families were made so that nobody's name falls back to
empty boxes, and a group is whoever is in it, writing in whatever script
they write. A sturdy book serif also reads like a printed club notice rather
than a tech product, and its round bowls and the ball on the r echo the
bear's head and ears. Noto is under the SIL Open Font License. Use the
files rather than retyping the word.

## Clear space and size

- Around the mark, keep clear space of a quarter of its width.
- Around the lockup, keep clear space of the height of the lowercase u.
- Use `logo-mark.svg` from 24 px up. Between 16 and 24 px use `favicon.svg`,
  which has a bigger face and its eyes set on the 16 px grid.
- Show the lockup at 96 px wide or more on screen, 30 mm or more in print.
- Print the one-colour mark at 8 mm or more.

## Files

| File | Use |
|---|---|
| `logo-mark.svg` | The mark from 24 px up: docs, slides, social cards, stickers. |
| `favicon.svg` | Browser tabs and anything from 16 to 32 px, including the admin and compliance pages. |
| `logo-avatar.svg` | The bot's profile picture on Discord, Slack and Telegram. The bear is fitted inside a circle on cream, so a round crop keeps the ears. |
| `logo-mark-mono.svg` | One colour: print, stamps, embroidery, engraving. Ink by default. Inline, set `--ours-mono` to recolour it; on dark grounds use cream. |
| `wordmark-light.svg` | The lockup on light pages, and the README header. |
| `wordmark-dark.svg` | The lockup on dark pages. |
| `mark.svg` | A copy of `logo-mark.svg` under the old placeholder's name. |

Chat platforms take a PNG for the avatar:

```sh
magick -background none -density 1152 docs/assets/logo-avatar.svg -resize 512x512 avatar.png
```

## Keep

- The tilt toward the word, the oval eyes and the closed face. No mouth,
  outline, shadow, gradient or highlight.
- Clay for the bear, except in the one-colour file.
- The bear on its own. It is never put inside a speech bubble, a badge or a
  frame other than the avatar's.
