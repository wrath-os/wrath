# Helpers modules load with `load("lib/merge.star", "merge")`.

def merge(a, b):
    """`b` merged into `a`: nested dictionaries merge, and anything else in `b` wins."""
    out = dict(a)
    for key, v in b.items():
        if type(v) == "dict" and type(out.get(key)) == "dict":
            out[key] = merge(out[key], v)
        else:
            out[key] = v
    return out
