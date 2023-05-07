terraform {
  required_version = ">= 1.2.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 4.34"
    }
  }
}

locals {
  name = "tower-aws-sample"
}

resource "null_resource" "this" {
  triggers = tomap({
    for file in fileset(path.module, "**/*.{lock,toml,rs}") :
    "${file}" => filesha256("${path.module}/${file}")
    if !startswith("${file}", "target/")
  })

  provisioner "local-exec" {
    working_dir = path.module
    command     = <<EOT
cargo lambda build --arm64 --example sample --release
install -D \
${path.module}/target/aarch64-unknown-linux-gnu/release/examples/sample \
${path.module}/target/lambda/sample/bootstrap
EOT
  }
}

data "archive_file" "this" {
  type        = "zip"
  source_file = "${path.module}/target/lambda/sample/bootstrap"
  output_path = "${path.module}/target/lambda/sample/bootstrap.zip"
  depends_on  = [null_resource.this]
}

resource "aws_kms_key" "this" {}

data "aws_iam_policy_document" "assume_role" {
  statement {
    effect = "Allow"
    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
    actions = ["sts:AssumeRole"]
  }
}

data "aws_iam_policy_document" "role" {
  statement {
    effect = "Allow"
    actions = [
      "kms:Decrypt",
      "kms:Encrypt",
    ]
    resources = [aws_kms_key.this.arn]
  }
}

resource "aws_iam_role" "this" {
  assume_role_policy = data.aws_iam_policy_document.assume_role.json
}

resource "aws_iam_role_policy" "this" {
  role   = aws_iam_role.this.name
  policy = data.aws_iam_policy_document.role.json
}

resource "aws_lambda_function" "this" {
  function_name    = local.name
  role             = aws_iam_role.this.arn
  filename         = data.archive_file.this.output_path
  source_code_hash = data.archive_file.this.output_base64sha256
  runtime          = "provided.al2"
  architectures    = ["arm64"]
  handler          = "handler"
  environment {
    variables = {
      KMS_KEY_ID = aws_kms_key.this.key_id
    }
  }
}

resource "aws_lambda_permission" "this" {
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.this.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.this.execution_arn}/*/$default"
}

resource "aws_apigatewayv2_api" "this" {
  name          = local.name
  protocol_type = "HTTP"
}

resource "aws_apigatewayv2_integration" "this" {
  api_id                 = aws_apigatewayv2_api.this.id
  integration_type       = "AWS_PROXY"
  integration_uri        = aws_lambda_function.this.invoke_arn
  payload_format_version = "2.0"
}

resource "aws_apigatewayv2_route" "this" {
  api_id    = aws_apigatewayv2_api.this.id
  route_key = "$default"
  target    = "integrations/${aws_apigatewayv2_integration.this.id}"
}

resource "aws_apigatewayv2_stage" "this" {
  api_id      = aws_apigatewayv2_api.this.id
  name        = "$default"
  auto_deploy = true
}
