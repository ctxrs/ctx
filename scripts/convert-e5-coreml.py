#!/usr/bin/env python3
import argparse
import types
from pathlib import Path
from typing import Optional, Tuple

import coremltools as ct
import numpy as np
import torch
import torch.nn.functional as functional
from transformers import AutoModel


MODEL_ID = "intfloat/multilingual-e5-small"
MODEL_REVISION = "614241f622f53c4eeff9890bdc4f31cfecc418b3"


def finite_encoder_attention_mask(
    model: torch.nn.Module,
    attention_mask: torch.Tensor,
    input_shape: Tuple[int, ...],
    device: Optional[torch.device] = None,
    dtype: Optional[torch.dtype] = None,
) -> torch.Tensor:
    del input_shape, device
    if attention_mask.dim() != 2 or model.config.is_decoder:
        raise ValueError("Core ML E5 conversion expects a two-dimensional encoder mask")
    dtype = dtype or model.dtype
    extended = attention_mask[:, None, None, :].to(dtype=dtype)
    return (1.0 - extended) * -10000.0


class E5Embedding(torch.nn.Module):
    def __init__(self) -> None:
        super().__init__()
        self.model = AutoModel.from_pretrained(MODEL_ID, revision=MODEL_REVISION)
        self.model.get_extended_attention_mask = types.MethodType(
            finite_encoder_attention_mask, self.model
        )
        self.model.eval()

    def forward(
        self,
        input_ids: torch.Tensor,
        attention_mask: torch.Tensor,
        token_type_ids: torch.Tensor,
    ) -> torch.Tensor:
        hidden = self.model(
            input_ids=input_ids,
            attention_mask=attention_mask,
            token_type_ids=token_type_ids,
            return_dict=False,
        )[0]
        mask = attention_mask.unsqueeze(-1).to(hidden.dtype)
        pooled = (hidden * mask).sum(dim=1) / mask.sum(dim=1).clamp(min=1.0)
        return functional.normalize(pooled, p=2, dim=1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Convert multilingual E5 with pooling to a fixed-shape Core ML program."
    )
    parser.add_argument("output", type=Path)
    parser.add_argument("--batch", type=int, required=True)
    parser.add_argument("--sequence", type=int, default=512)
    parser.add_argument(
        "--precision", choices=("fp16", "fp32", "mixed"), default="fp16"
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.batch <= 0 or not 1 <= args.sequence <= 512:
        raise SystemExit("--batch must be positive and --sequence must be in 1..=512")

    model = E5Embedding().eval()
    shape = (args.batch, args.sequence)
    attention_mask = torch.zeros(shape, dtype=torch.int32)
    attention_mask[:, : min(32, args.sequence)] = 1
    example = (
        torch.zeros(shape, dtype=torch.int32),
        attention_mask,
        torch.zeros(shape, dtype=torch.int32),
    )
    with torch.inference_mode():
        traced = torch.jit.trace(model, example, strict=True)
        expected = model(*example).detach().cpu().numpy()

    inputs = [
        ct.TensorType(name="input_ids", shape=shape, dtype=np.int32),
        ct.TensorType(name="attention_mask", shape=shape, dtype=np.int32),
        ct.TensorType(name="token_type_ids", shape=shape, dtype=np.int32),
    ]
    if args.precision == "fp16":
        precision = ct.precision.FLOAT16
    elif args.precision == "fp32":
        precision = ct.precision.FLOAT32
    else:
        precision = ct.transform.FP16ComputePrecision(
            op_selector=lambda operation: operation.op_type in {"linear", "matmul"}
        )
    program = ct.convert(
        traced,
        convert_to="mlprogram",
        inputs=inputs,
        outputs=[ct.TensorType(name="sentence_embeddings", dtype=np.float32)],
        compute_precision=precision,
        minimum_deployment_target=ct.target.macOS13,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    program.save(str(args.output))

    inputs = {
        "input_ids": example[0].numpy(),
        "attention_mask": example[1].numpy(),
        "token_type_ids": example[2].numpy(),
    }
    minimum_cosine = 1.0
    for name, compute_units in (
        ("cpu", ct.ComputeUnit.CPU_ONLY),
        ("all", ct.ComputeUnit.ALL),
    ):
        loaded = ct.models.MLModel(str(args.output), compute_units=compute_units)
        prediction = loaded.predict(inputs)["sentence_embeddings"]
        if not np.all(np.isfinite(prediction)):
            raise SystemExit(f"Core ML conversion produced non-finite {name} output")
        cosine = np.sum(expected * prediction, axis=1) / (
            np.linalg.norm(expected, axis=1) * np.linalg.norm(prediction, axis=1)
        )
        if not np.all(np.isfinite(cosine)):
            raise SystemExit(f"Core ML conversion produced non-finite {name} cosine")
        minimum_cosine = min(minimum_cosine, float(cosine.min()))
        print(
            f"verify={name} minimum_cosine={cosine.min():.8f} "
            f"max_abs={np.max(np.abs(expected - prediction)):.8f}"
        )
    print(
        f"wrote {args.output} batch={args.batch} sequence={args.sequence} "
        f"precision={args.precision}"
    )
    if minimum_cosine < 0.999:
        raise SystemExit(f"Core ML conversion failed cosine gate: {minimum_cosine:.8f}")


if __name__ == "__main__":
    main()
