"""Draws the XSnes3D+ launcher icon into .github/fork/res (run it again after a change).

A console-grey tile with the SNES's four face buttons in a diamond: the dark purple A and B,
the lavender X and Y, and a small "3D" plus in the corner, all in square pixels.
"""

from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
SIZES = {'mdpi': 48, 'hdpi': 72, 'xhdpi': 96, 'xxhdpi': 144, 'xxxhdpi': 192}

GREY = (196, 196, 204, 255)
DARK = (60, 60, 68, 255)
PURPLE = (84, 69, 148, 255)
LAVENDER = (153, 140, 209, 255)
BLACK = (18, 16, 24, 255)


def draw(size):
    scale = 4
    big = size * scale
    image = Image.new('RGBA', (big, big), (0, 0, 0, 0))
    pen = ImageDraw.Draw(image)
    margin = big * 0.06
    pen.rounded_rectangle([margin, margin, big - margin, big - margin], radius=big * 0.18,
                          fill=GREY, outline=DARK, width=int(big * 0.03))
    centre = big / 2
    gap = big * 0.2
    radius = big * 0.11
    # Top X, left Y (lavender); right A, bottom B (purple), as on the pad.
    for (dx, dy), colour in [((0, -1), LAVENDER), ((-1, 0), LAVENDER), ((1, 0), PURPLE),
                             ((0, 1), PURPLE)]:
        x, y = centre + dx * gap, centre + dy * gap
        pen.ellipse([x - radius, y - radius, x + radius, y + radius], fill=colour,
                    outline=BLACK, width=int(big * 0.015))
    # The plus in the middle.
    arm, thick = big * 0.06, big * 0.022
    pen.rectangle([centre - arm, centre - thick, centre + arm, centre + thick], fill=BLACK)
    pen.rectangle([centre - thick, centre - arm, centre + thick, centre + arm], fill=BLACK)
    return image.resize((size, size), Image.LANCZOS)


for name, size in SIZES.items():
    folder = HERE / 'res' / f'mipmap-{name}'
    folder.mkdir(parents=True, exist_ok=True)
    draw(size).save(folder / 'ic_launcher.png')
print('icon drawn')
