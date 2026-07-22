#!/usr/bin/env python3
"""Shared Transformers runtime for trained Atelier VLM adapters."""

import copy

from vlm_data import extract_json_object


def inference_dtype(torch):
    if not torch.cuda.is_available():
        return torch.float32
    if torch.cuda.is_bf16_supported():
        return torch.bfloat16
    return torch.float16


def multimodal_model_class(transformers):
    model_class = getattr(transformers, "AutoModelForMultimodalLM", None)
    if model_class is None:
        raise ValueError(
            "installed transformers does not provide AutoModelForMultimodalLM; "
            "install requirements-vlm.txt"
        )
    return model_class


def generation_chat_template_kwargs(model):
    model_type = getattr(getattr(model, "config", None), "model_type", "")
    if model_type in ("qwen3_5", "qwen3_5_moe"):
        return {"enable_thinking": False}
    return {}


def load_model(base_model, adapter=None, quantization="none"):
    try:
        import torch
        import transformers
        from peft import PeftModel
        from transformers import AutoProcessor, BitsAndBytesConfig
    except ImportError as error:
        raise ValueError(
            "inference dependencies missing; install requirements-vlm.txt"
        ) from error

    dtype = inference_dtype(torch)
    kwargs = {"dtype": dtype, "device_map": "auto"}
    if quantization == "4bit":
        if not torch.cuda.is_available():
            raise ValueError("4bit inference currently requires a CUDA device")
        kwargs["quantization_config"] = BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_quant_type="nf4",
            bnb_4bit_compute_dtype=dtype,
        )
    elif quantization != "none":
        raise ValueError("quantization must be none or 4bit")
    model_class = multimodal_model_class(transformers)
    model = model_class.from_pretrained(base_model, **kwargs)
    if adapter:
        model = PeftModel.from_pretrained(model, adapter)
    model.eval()
    processor = AutoProcessor.from_pretrained(adapter or base_model)
    return model, processor


def attach_images(messages, images):
    messages = copy.deepcopy(messages)
    iterator = iter(images)
    count = 0
    for message in messages:
        if not isinstance(message.get("content"), list):
            continue
        for item in message["content"]:
            if item.get("type") == "image":
                try:
                    item["image"] = next(iterator)
                except StopIteration as error:
                    raise ValueError("fewer images than prompt placeholders") from error
                count += 1
    try:
        next(iterator)
    except StopIteration:
        pass
    else:
        raise ValueError("more images than prompt placeholders")
    if count == 0:
        raise ValueError("prompt contains no image placeholders")
    return messages


def generate_json(model, processor, messages, required_key, max_new_tokens):
    import torch

    inputs = processor.apply_chat_template(
        messages,
        tokenize=True,
        add_generation_prompt=True,
        return_dict=True,
        return_tensors="pt",
        **generation_chat_template_kwargs(model),
    )
    inputs = inputs.to(model.device)
    with torch.inference_mode():
        generated = model.generate(
            **inputs,
            max_new_tokens=max_new_tokens,
            do_sample=False,
        )
    trimmed = [output[len(source) :] for source, output in zip(inputs.input_ids, generated)]
    text = processor.batch_decode(
        trimmed, skip_special_tokens=True, clean_up_tokenization_spaces=False
    )[0]
    return extract_json_object(text, required_key)
